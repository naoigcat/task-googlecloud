require "minitest/autorun"
require "pathname"
require "securerandom"
require "shellwords"
require "stringio"
require "tmpdir"
require_relative "../lib/upload"
require_relative "interrupt_test_helper"

class UploadInterruptTransactionTest < Minitest::Test
  include InterruptTestHelper

  def test_call_restores_remote_and_local_paths_when_interrupted_after_finalization
    with_unicode_upload do |directory, file, normalized_file|
      commands, staging_path, final_path = run_interrupted_upload(directory, file, :finalization)

      assert_equal "content", file.read
      refute normalized_file.exist?
      assert_equal expected_finalization_commands(normalized_file, staging_path, final_path), commands
    end
  end

  def test_call_restores_staged_upload_when_interrupted_after_the_copy
    with_unicode_upload do |directory, file, normalized_file|
      commands, staging_path, = run_interrupted_upload(directory, file, :staging)

      assert_equal "content", file.read
      refute normalized_file.exist?
      assert_equal expected_staging_commands(normalized_file, staging_path), commands
    end
  end

  private

  def run_interrupted_upload(directory, file, phase)
    staging_path = "gs://bucket/.task-googlecloud-staging/token/é.txt"
    final_path = "gs://bucket/é.txt"
    commands = []
    generation_paths = expected_generation_paths(staging_path, final_path, phase)

    with_interrupt_after_side_effect do |trigger|
      exec = interrupted_exec(commands, trigger, staging_path, final_path, phase)
      run_upload_with_interrupt(directory, file, exec, generation_pipe(generation_paths))
    end
    assert_empty generation_paths
    [commands, staging_path, final_path]
  end

  def expected_generation_paths(staging_path, final_path, phase)
    return [staging_path] if phase == :staging

    [staging_path, final_path]
  end

  def run_upload_with_interrupt(directory, file, exec, pipe)
    assert_raises(Interrupt) { run_upload(directory, file, exec, pipe) }
  end

  def interrupted_exec(commands, trigger, staging_path, final_path, phase)
    lambda do |command|
      commands << command
      trigger.call if interrupted_command?(command, staging_path, final_path, phase)
    end
  end

  def interrupted_command?(command, staging_path, final_path, phase)
    return command.start_with?("gsutil cp") if phase == :staging

    command == Cloud::ObjectMove.command(staging_path, final_path)
  end

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

  def run_upload(directory, file, exec, pipe)
    uploader = Cloud::Upload.new("project")
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

  def generation_pipe(generation_paths)
    lambda do |command, &block|
      expected_path = generation_paths.shift
      assert_equal Shellwords.join(["gsutil", "stat", expected_path]), command
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

  def expected_finalization_commands(normalized_file, staging_path, final_path)
    expected_commands(normalized_file, staging_path) + [
      Cloud::ObjectMove.rollback_command(staging_path, final_path, "101"),
      Cloud::ObjectMove.cleanup_command(staging_path, "101"),
    ]
  end

  def expected_staging_commands(normalized_file, staging_path)
    expected_commands(normalized_file, staging_path).first(2) + [Cloud::ObjectMove.cleanup_command(staging_path, "101")]
  end
end
