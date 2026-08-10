require "minitest/autorun"
require "pathname"
require "securerandom"
require "shellwords"
require "stringio"
require "tmpdir"
require_relative "../lib/upload"

class UploadTransactionTest < Minitest::Test
  def test_call_restores_local_paths_when_an_upload_fails
    with_unicode_upload do |directory, file, normalized_file|
      uploader = Cloud::Upload.new("project")
      failing_exec =
        ->(command) { raise StandardError, "upload failed" if command.start_with?("gsutil cp") }

      assert_raises(StandardError) { run_upload(uploader, directory, file, failing_exec) }
      assert_equal "content", file.read
      refute normalized_file.exist?
    end
  end

  def test_call_uses_the_normalized_filename_for_remote_paths
    with_unicode_upload do |directory, file, normalized_file|
      commands = []
      uploader = Cloud::Upload.new("project")
      recording_exec = ->(command) { commands << command }
      run_upload(uploader, directory, file, recording_exec)

      staging_path = "gs://bucket/.task-googlecloud-staging/token/é.txt"
      assert_equal expected_commands(normalized_file, staging_path), commands
    end
  end

  def test_call_restores_staged_objects_when_finalization_fails
    with_unicode_upload do |directory, file, normalized_file|
      staging_path = "gs://bucket/.task-googlecloud-staging/token/é.txt"
      final_path = "gs://bucket/é.txt"
      commands = []
      uploader = Cloud::Upload.new("project")

      assert_raises(StandardError) do
        run_upload(uploader, directory, file, failing_finalize_exec(commands, staging_path, final_path))
      end

      assert_equal expected_failure_commands(normalized_file, staging_path, final_path), commands
    end
  end

  def test_call_reports_manual_cleanup_when_generation_lookup_fails_after_upload
    with_unicode_upload do |directory, file, _normalized_file|
      uploader = Cloud::Upload.new("project")
      generation_error = Cloud::ObjectMove::MissingGenerationError.new("gs://bucket/staged")

      error =
        assert_raises(NormalizationPlan::RollbackError) do
          run_upload(uploader, directory, file, ->(_command) {}, ->(_command, &) { raise generation_error })
        end

      assert_includes error.message, "Cannot verify ownership"
    end
  end

  private

  def with_unicode_upload
    Dir.mktmpdir do |directory_path|
      directory = Pathname.new(directory_path).join("bucket")
      directory.mkdir
      file = directory.join("é.txt")
      normalized_file = directory.join("é.txt")
      file.write("content")
      skip "The filesystem does not preserve distinct Unicode names" if normalized_file.exist?

      yield directory, file, normalized_file
    end
  end

  def run_upload(uploader, directory, file, exec, pipe = generation_pipe)
    uploader.stub(:upload_files_by_directory, { directory => [file] }) do
      Cloud.stub(:login, nil) do
        SecureRandom.stub(:hex, "token") do
          Cloud.stub(:pipe, pipe) do
            Cloud.stub(:exec, exec) { uploader.call }
          end
        end
      end
    end
  end

  def generation_pipe
    lambda do |_command, &block|
      block.call(StringIO.new("Generation: 101\n"))
    end
  end

  def expected_commands(normalized_file, staging_path)
    [
      Shellwords.join(%w[gcloud config set project project]),
      Shellwords.join(["gsutil", "cp", normalized_file.to_path, staging_path]),
      Cloud::ObjectMove.command(staging_path, "gs://bucket/é.txt"),
    ]
  end

  def failing_finalize_exec(commands, staging_path, final_path)
    lambda do |command|
      commands << command
      raise StandardError, "finalization failed" if command == Cloud::ObjectMove.command(staging_path, final_path)
    end
  end

  def expected_failure_commands(normalized_file, staging_path, _final_path)
    expected_commands(normalized_file, staging_path) + [Cloud::ObjectMove.cleanup_command(staging_path, "101")]
  end
end
