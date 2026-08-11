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
      failing_copy = ->(_source, _target) { raise StandardError, "upload failed" }

      assert_raises(StandardError) { run_upload(uploader, directory, file, copy: failing_copy) }
      assert_equal "content", file.read
      refute normalized_file.exist?
    end
  end

  def test_call_uses_the_normalized_filename_for_remote_paths
    with_unicode_upload do |directory, file, normalized_file|
      commands = []
      run_upload(Cloud::Upload.new("project"), directory, file, commands: commands)

      assert_equal expected_commands(normalized_file), commands
    end
  end

  def test_call_restores_staged_objects_when_finalization_fails
    with_unicode_upload do |directory, file, normalized_file|
      commands = []
      error = StandardError.new("finalization failed")
      move = failing_move(commands, error)

      assert_raises(StandardError) do
        run_upload(Cloud::Upload.new("project"), directory, file, commands: commands, move: move)
      end

      assert_equal expected_commands(normalized_file) + [cleanup_command], commands
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

  def run_upload(uploader, directory, file, commands: [], **stubs)
    uploader.stub(:upload_files_by_directory, { directory => [file] }) do
      with_upload_stubs(commands, **stubs) { uploader.call }
    end
  end

  def with_upload_stubs(
    commands,
    copy: recording_copy(commands),
    move: recording_move(commands),
    pipe: generation_pipe,
    &
  )
    Cloud.stub(:login, nil) do
      SecureRandom.stub(:hex, "token") do
        Cloud::ObjectCopy.stub(:copy, copy) do
          Cloud::ObjectMove.stub(:move, move) do
            Cloud.stub(:pipe, pipe) { Cloud.stub(:exec, recording_exec(commands), &) }
          end
        end
      end
    end
  end

  def recording_copy(commands)
    lambda do |source, target|
      commands << Cloud::ObjectCopy.command(source, target)
      "101"
    end
  end

  def recording_move(commands)
    lambda do |source, target|
      commands << Cloud::ObjectMove.command("#{source}#101", target, source_path: source)
      "102"
    end
  end

  def failing_move(commands, error)
    lambda do |source, target|
      commands << Cloud::ObjectMove.command("#{source}#101", target, source_path: source)
      raise error
    end
  end

  def recording_exec(commands) = ->(command) { commands << command }

  def generation_pipe = ->(_command, &block) { block.call(StringIO.new("Generation: 101\n")) }

  def expected_commands(normalized_file)
    [
      Shellwords.join(%w[gcloud config set project project]),
      Cloud::ObjectCopy.command(normalized_file.to_path, staging_path),
      Cloud::ObjectMove.command("#{staging_path}#101", final_path, source_path: staging_path),
    ]
  end

  def staging_path = "gs://bucket/.task-googlecloud-staging/token/é.txt"

  def final_path = "gs://bucket/é.txt"

  def cleanup_command = Cloud::ObjectMove.cleanup_command(staging_path, "101")
end
