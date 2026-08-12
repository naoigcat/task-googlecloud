require "shellwords"
require_relative "cloud_storage_api"
require_relative "object_copy"
require_relative "object_move_commands"

module Cloud
  module ObjectMove
    class MissingGenerationError < StandardError
      def initialize(target)
        super("Cannot verify ownership for #{target.inspect} without a generation")
      end
    end

    class RecoveryRequiredError < StandardError
      def initialize(source, target, operation_error, details)
        paths = [source, target].compact.map(&:inspect).join(" and ")
        super("Manual recovery required for #{paths} after #{operation_error.message}; #{details}")
      end
    end

    module_function

    extend Cloud::ObjectMoveCommands

    def command(source, target, source_path: source, source_generation: nil)
      move = move_command(source, target, source_path, source_generation)
      source_state = state_assignment("source_state", source_path)

      "{ #{move}; move_status=$?; [ \"$move_status\" -eq 0 ] || exit \"$move_status\"; " \
        "#{source_state}[ \"$source_state\" = missing ] || exit 1; } 2>&1"
    end

    def move_command(source, target, source_path, source_generation)
      return Cloud::StorageApi.move_command(source, target, source_generation:) if special_move?(
        source,
        target,
        source_path,
      )

      source_version = source_generation ? "#{source}##{source_generation}" : source
      Shellwords.join(%w[gsutil -h x-goog-if-generation-match:0 mv -n -v] + [source_version, target])
    end

    def special_move?(source, target, source_path)
      [source, target, source_path].any? { |path| Cloud::StorageApi.special_name?(path) }
    end

    def move(source, target)
      source_generation = generation(source)
      ObjectCopy.receipt(Cloud.pipe(command(source, target, source_path: source, source_generation:), &:read), target)
    end

    def generation(path)
      output = Cloud.pipe(stat_command(path), &:read)
      generation = output[/^\s*Generation:\s*(\d+)\s*$/, 1]
      generation || raise(MissingGenerationError, path)
    end

    def stat_command(path)
      return Cloud::StorageApi.stat_command(path) if Cloud::StorageApi.special_name?(path)

      Shellwords.join(["gsutil", "stat", path])
    end

    def confirm_move_after_failure(source, target, operation_error)
      source_details = object_details(source)
      target_details = object_details(target)
      confirm_move_was_not_applied(source, target, operation_error, source_details, target_details)
    rescue StandardError => e
      raise if e.is_a?(RecoveryRequiredError)

      raise RecoveryRequiredError.new(source, target, operation_error, "state confirmation failed: #{e.message}")
    end

    def confirm_write_after_failure(target, operation_error)
      target_details = object_details(target)
      # A failed remote command can still reach Cloud Storage after this probe.
      # An empty target therefore cannot prove the write was not applied.
      return if target_details.first == :missing && !operation_error.is_a?(Cloud::CommandError)

      raise RecoveryRequiredError.new(nil, target, operation_error, state_details(target, *target_details))
    rescue StandardError => e
      raise if e.is_a?(RecoveryRequiredError)

      raise RecoveryRequiredError.new(nil, target, operation_error, "state confirmation failed: #{e.message}")
    end

    def rollback_command(source, target, target_generation)
      raise MissingGenerationError, target unless target_generation
      "#{rollback_state_assignments(source, target, target_generation)}" \
        "#{rollback_outcome(source, target, target_generation)} 2>&1"
    end

    def rollback(source, target, target_generation)
      ObjectCopy.receipt(Cloud.pipe(rollback_command(source, target, target_generation), &:read), source)
    end

    def cleanup_command(target, target_generation)
      raise MissingGenerationError, target unless target_generation
      remove_exact_target = cleanup_delete_command(target, target_generation)
      exact_target_state = state_assignment("exact_target_state", target, target_generation)
      target_state = state_assignment("target_state", target)

      "#{exact_target_state}if [ \"$exact_target_state\" = present ]; then #{remove_exact_target}; " \
        "#{target_state}[ \"$target_state\" = missing ] || exit 1; " \
        "elif [ \"$exact_target_state\" = missing ]; then " \
        "#{target_state}[ \"$target_state\" = missing ] || exit 1; else exit 1; fi"
    end

    def cleanup_delete_command(target, target_generation)
      return Cloud::StorageApi.delete_command(target, target_generation) if Cloud::StorageApi.special_name?(target)

      Shellwords.join(["gsutil", "rm", "#{target}##{target_generation}"])
    end
  end
end
