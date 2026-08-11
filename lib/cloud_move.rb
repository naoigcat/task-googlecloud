require "shellwords"
require_relative "object_copy"

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

    def command(source, target, source_path: source)
      move = Shellwords.join(["gsutil", "-h", "x-goog-if-generation-match:0", "mv", "-n", "-v", source, target])
      source_state = state_assignment("source_state", source_path)

      "{ #{move}; move_status=$?; [ \"$move_status\" -eq 0 ] || exit \"$move_status\"; " \
        "#{source_state}[ \"$source_state\" = missing ] || exit 1; } 2>&1"
    end

    def move(source, target)
      source_generation = generation(source)
      source_version = generation_path(source, source_generation)
      ObjectCopy.receipt(Cloud.pipe(command(source_version, target, source_path: source), &:read), target)
    end

    def generation(path)
      output = Cloud.pipe(Shellwords.join(["gsutil", "stat", path]), &:read)
      generation = output[/^\s*Generation:\s*(\d+)\s*$/, 1]
      return generation if generation

      raise MissingGenerationError, path
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
      target_at_generation = generation_path(target, target_generation)
      "#{rollback_state_assignments(source, target_at_generation)}" \
        "#{rollback_outcome(source, target, target_at_generation)} 2>&1"
    end

    def rollback(source, target, target_generation)
      ObjectCopy.receipt(Cloud.pipe(rollback_command(source, target, target_generation), &:read), source)
    end

    def cleanup_command(target, target_generation)
      raise MissingGenerationError, target unless target_generation
      target_at_generation = generation_path(target, target_generation)
      remove_exact_target = Shellwords.join(["gsutil", "rm", target_at_generation])
      exact_target_state = state_assignment("exact_target_state", target_at_generation)
      target_state = state_assignment("target_state", target)

      "#{exact_target_state}if [ \"$exact_target_state\" = present ]; then #{remove_exact_target}; " \
        "#{target_state}[ \"$target_state\" = missing ] || exit 1; " \
        "elif [ \"$exact_target_state\" = missing ]; then " \
        "#{target_state}[ \"$target_state\" = missing ] || exit 1; else exit 1; fi"
    end

    def generation_path(path, generation) = "#{path}##{generation}"

    def object_details(path)
      state(path).then { |current_state| [current_state, current_state == :present ? generation(path) : nil] }
    end

    def state(path)
      output = Cloud.pipe(state_command(path), &:read).strip
      return output.to_sym if %w[present missing].include?(output)

      raise MissingGenerationError, path
    end

    def state_command(path)
      stat = Shellwords.join(["env", "LC_ALL=C", "gsutil", "stat", path])
      missing_output = Shellwords.escape("No URLs matched: #{path}")

      "output=$(#{stat} 2>&1); status=$?; " \
        "if [ \"$status\" -eq 0 ]; then printf present; " \
        "elif [ \"$status\" -eq 1 ] && [ \"$output\" = #{missing_output} ]; then printf missing; " \
        "else printf '%s\\n' \"$output\" >&2; exit \"$status\"; fi"
    end

    def state_assignment(variable, path) = "#{variable}=$(#{state_command(path)}) || exit $?; "

    def state_details(path, current_state, current_generation)
      current_state == :missing ? "#{path.inspect} is missing" : "#{path.inspect}: generation #{current_generation}"
    end

    def rollback_state_assignments(source, target_at_generation)
      state_assignment("source_state", source) + state_assignment("exact_target_state", target_at_generation)
    end

    def rollback_outcome(source, target, target_at_generation)
      copy_back = Shellwords.join(
        ["gsutil", "-h", "x-goog-if-generation-match:0", "cp", "-v", target_at_generation, source],
      )
      remove_exact_target = Shellwords.join(["gsutil", "rm", target_at_generation])

      "if [ \"$source_state\" = missing ] && [ \"$exact_target_state\" = present ]; then " \
        "#{run_or_exit(copy_back, "copy_status")}#{run_or_exit(remove_exact_target, "remove_status")}" \
        "#{state_assignment("target_state", target)}[ \"$target_state\" = missing ] || exit 1; " \
        "else exit 1; fi"
    end

    def run_or_exit(command, status_variable)
      "#{command}; #{status_variable}=$?; [ \"$#{status_variable}\" -eq 0 ] || exit \"$#{status_variable}\"; "
    end

    # A command-status failure can arrive at Cloud Storage after its local result.
    # Current paths therefore cannot prove the move was not applied.
    def confirm_move_was_not_applied(source, target, operation_error, source_details, target_details)
      no_change = source_details.first == :present && target_details.first == :missing
      return if no_change && !operation_error.is_a?(Cloud::CommandError)

      details = [state_details(source, *source_details), state_details(target, *target_details)].join("; ")
      raise RecoveryRequiredError.new(source, target, operation_error, details)
    end
  end
end
