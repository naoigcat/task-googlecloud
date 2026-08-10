require "shellwords"

module Cloud
  module ObjectMove
    class MissingGenerationError < StandardError
      def initialize(target)
        super("Cannot verify ownership for #{target.inspect} without a generation")
      end
    end

    module_function

    def command(source, target)
      move = Shellwords.join(["gsutil", "mv", "-n", source, target])
      source_stat = Shellwords.join(["gsutil", "-q", "stat", source])
      "#{move} && ! #{source_stat}"
    end

    def generation(path)
      output = Cloud.pipe(Shellwords.join(["gsutil", "stat", path]), &:read)
      generation = output[/^\s*Generation:\s*(\d+)\s*$/, 1]
      return generation if generation

      raise MissingGenerationError, path
    end

    def rollback_command(source, target, target_generation)
      raise MissingGenerationError, target unless target_generation

      source_stat = Shellwords.join(["gsutil", "-q", "stat", source])
      target_stat = Shellwords.join(["gsutil", "-q", "stat", target])
      target_at_generation = generation_path(target, target_generation)
      exact_target_stat = Shellwords.join(["gsutil", "-q", "stat", target_at_generation])
      copy_back = Shellwords.join(["gsutil", "-h", "x-goog-if-generation-match:0", "cp", target_at_generation, source])
      remove_exact_target = Shellwords.join(["gsutil", "rm", "-f", target_at_generation])

      "if ! #{source_stat} && #{exact_target_stat}; then " \
        "#{copy_back} && #{remove_exact_target} && ! #{target_stat}; " \
        "elif #{source_stat} && ! #{exact_target_stat}; then exit 0; else exit 1; fi"
    end

    def cleanup_command(target, target_generation)
      raise MissingGenerationError, target unless target_generation

      target_stat = Shellwords.join(["gsutil", "-q", "stat", target])
      target_at_generation = generation_path(target, target_generation)
      exact_target_stat = Shellwords.join(["gsutil", "-q", "stat", target_at_generation])
      remove_exact_target = Shellwords.join(["gsutil", "rm", "-f", target_at_generation])

      "if #{exact_target_stat}; then #{remove_exact_target}; " \
        "elif ! #{target_stat}; then exit 0; else exit 1; fi"
    end

    def generation_path(path, generation)
      "#{path}##{generation}"
    end
  end
end
