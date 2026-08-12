require "shellwords"
require_relative "cloud_storage_api"

module Cloud
  module ObjectMoveCommands
    def object_details(path)
      state(path).then { |current_state| [current_state, current_state == :present ? generation(path) : nil] }
    end

    def state(path)
      output = Cloud.pipe(state_command(path), &:read).strip
      return output.to_sym if %w[present missing].include?(output)

      raise Cloud::ObjectMove::MissingGenerationError, path
    end

    def state_command(path, gen = nil)
      return Cloud::StorageApi.state_command(path, generation: gen) if Cloud::StorageApi.special_name?(path)

      uri = gen ? "#{path}##{gen}" : path
      stat = Shellwords.join(["env", "LC_ALL=C", "gsutil", "stat", uri])
      missing_output = Shellwords.escape("No URLs matched: #{uri}")

      "output=$(#{stat} 2>&1); status=$?; " \
        "if [ \"$status\" -eq 0 ]; then printf present; " \
        "elif [ \"$status\" -eq 1 ] && [ \"$output\" = #{missing_output} ]; then printf missing; " \
        "else printf '%s\\n' \"$output\" >&2; exit \"$status\"; fi"
    end

    def state_assignment(variable, path, gen = nil) = "#{variable}=$(#{state_command(path, gen)}) || exit $?; "

    def state_details(path, current_state, current_generation)
      current_state == :missing ? "#{path.inspect} is missing" : "#{path.inspect}: generation #{current_generation}"
    end

    def rollback_state_assignments(source, target, target_generation)
      state_assignment("source_state", source) + state_assignment("exact_target_state", target, target_generation)
    end

    def rollback_outcome(source, target, target_generation)
      copy_back, remove_exact_target =
        if [source, target].any? { |path| Cloud::StorageApi.special_name?(path) }
          special_rollback_commands(source, target, target_generation)
        else
          gsutil_rollback_commands(source, target, target_generation)
        end

      "if [ \"$source_state\" = missing ] && [ \"$exact_target_state\" = present ]; then " \
        "#{run_or_exit(copy_back, "copy_status")}#{run_or_exit(remove_exact_target, "remove_status")}" \
        "#{state_assignment("target_state", target)}[ \"$target_state\" = missing ] || exit 1; " \
        "else exit 1; fi"
    end

    def special_rollback_commands(source, target, target_generation)
      [
        Cloud::StorageApi.copy_command(
          target,
          source,
          source_generation: target_generation,
          destination_generation: "0",
        ),
        Cloud::StorageApi.delete_command(target, target_generation),
      ]
    end

    def gsutil_rollback_commands(source, target, target_generation)
      target_at_generation = "#{target}##{target_generation}"
      [
        Shellwords.join(%w[gsutil -h x-goog-if-generation-match:0 cp -v] + [target_at_generation, source]),
        Shellwords.join(["gsutil", "rm", target_at_generation]),
      ]
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
      raise Cloud::ObjectMove::RecoveryRequiredError.new(source, target, operation_error, details)
    end
  end
end
