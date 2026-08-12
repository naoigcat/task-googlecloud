require "shellwords"
require_relative "cloud_storage_api"

module Cloud
  module ObjectCopy
    module_function

    def copy(source, target)
      receipt(Cloud.pipe(command(source, target), &:read), target)
    end

    def command(source, target)
      copy = special_name?(source, target) ? api_copy(source, target) : gsutil_copy(source, target)
      "#{copy} 2>&1"
    end

    def special_name?(source, target)
      Cloud::StorageApi.special_name?(source) || Cloud::StorageApi.special_name?(target)
    end

    def api_copy(source, target)
      return Cloud::StorageApi.upload_command(source, target) unless source.start_with?("gs://")

      Cloud::StorageApi.copy_command(source, target, source_generation: nil, destination_generation: "0")
    end

    def gsutil_copy(source, target)
      Shellwords.join(%w[gsutil -h x-goog-if-generation-match:0 cp -v] + [source, target])
    end

    def receipt(output, target)
      # Use the version URL emitted by the write itself so a later stat cannot adopt another run's object.
      pattern = /\A\s*Created:\s+#{Regexp.escape(target)}#(\d+)\s*\z/
      generation = output.each_line.filter_map { |line| line[pattern, 1] }
      generation = generation.last
      return generation if generation

      raise ObjectMove::MissingGenerationError, target
    end
  end
end
