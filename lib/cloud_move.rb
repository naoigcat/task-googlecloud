require "shellwords"

module Cloud
  module ObjectMove
    module_function

    def command(source, target)
      move = Shellwords.join(["gsutil", "mv", "-n", source, target])
      source_stat = Shellwords.join(["gsutil", "-q", "stat", source])
      "#{move} && ! #{source_stat}"
    end

    def rollback_command(source, target)
      source_stat = Shellwords.join(["gsutil", "-q", "stat", source])
      target_stat = Shellwords.join(["gsutil", "-q", "stat", target])
      move_back = Shellwords.join(["gsutil", "mv", "-n", target, source])

      "if ! #{source_stat} && #{target_stat}; then #{move_back} && ! #{target_stat}; " \
        "elif #{source_stat} && ! #{target_stat}; then exit 0; else exit 1; fi"
    end
  end
end
