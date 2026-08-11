require "shellwords"

module Cloud
  module ObjectCopy
    module_function

    def copy(source, target)
      receipt(Cloud.pipe(command(source, target), &:read), target)
    end

    def command(source, target)
      copy = Shellwords.join(["gsutil", "-h", "x-goog-if-generation-match:0", "cp", "-v", source, target])
      "#{copy} 2>&1"
    end

    def receipt(output, target)
      # Use the version URL emitted by the write itself so a later stat cannot adopt another run's object.
      pattern = /\A\s*Created:\s+#{Regexp.escape(target)}#(\d+)\s*\z/
      generation = output.each_line.filter_map { |line| line[pattern, 1] }
                         .last
      return generation if generation

      raise ObjectMove::MissingGenerationError, target
    end
  end
end
