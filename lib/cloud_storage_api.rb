require "shellwords"

module Cloud
  module StorageApi
    SCRIPT = "/app/docker/googlecloud-storage.py".freeze
    SPECIAL_NAME = /[*?\[\]#]/
    private_constant :SCRIPT, :SPECIAL_NAME

    module_function

    def special_name?(path) = path.match?(SPECIAL_NAME)

    def upload_command(source, target)
      bucket, object = split_uri(target)
      command("upload", "--file", source, "--bucket", bucket, "--object", object)
    end

    def copy_command(source, target, source_generation:, destination_generation:)
      command(
        "copy",
        *uri_arguments(source, target),
        *generation_option("--source-generation", source_generation),
        *generation_option("--destination-generation", destination_generation),
      )
    end

    def move_command(source, target, source_generation:)
      command("move", *uri_arguments(source, target), *generation_option("--source-generation", source_generation))
    end

    def stat_command(path, generation: nil)
      bucket, object = split_uri(path)
      command("stat", "--bucket", bucket, "--object", object, *generation_option("--generation", generation))
    end

    def state_command(path, generation: nil)
      bucket, object = split_uri(path)
      command("state", "--bucket", bucket, "--object", object, *generation_option("--generation", generation))
    end

    def list_command(bucket)
      command("list", "--bucket", bucket)
    end

    def delete_command(path, generation)
      bucket, object = split_uri(path)
      command("delete", "--bucket", bucket, "--object", object, "--generation", generation)
    end

    def split_uri(path)
      match = %r{\Ags://([^/]+)/(.+)\z}.match(path)
      raise ArgumentError, "Expected a Cloud Storage URI: #{path.inspect}" unless match

      [match[1], match[2]]
    end

    def command(*arguments)
      Shellwords.join(["python3", SCRIPT, *arguments])
    end

    def uri_arguments(source, target)
      source_bucket, source_object = split_uri(source)
      target_bucket, target_object = split_uri(target)
      pair("--source-bucket", source_bucket) + pair("--source-object", source_object) +
        pair("--target-bucket", target_bucket) + pair("--target-object", target_object)
    end

    def pair(name, value) = [name, value]

    def generation_option(name, generation)
      generation ? [name, generation] : []
    end
  end
end
