require "minitest/autorun"
require "pathname"
require "securerandom"
require "shellwords"
require "stringio"
require "tmpdir"
require_relative "../lib/upload"

class UploadFinalizationRollbackTest < Minitest::Test
  def test_call_cleans_recreated_staging_generation_when_finalization_fails_after_a_move
    with_two_unicode_upload do |directory, files, normalized_files|
      commands, generations, staging_paths, final_paths = run_finalization_failure(directory, files, normalized_files)

      assert_empty generations
      assert_equal expected_commands(normalized_files, staging_paths, final_paths), commands
    end
  end

  private

  def with_two_unicode_upload
    Dir.mktmpdir do |directory_path|
      directory = Pathname.new(directory_path).join("bucket")
      directory.mkdir
      files = [directory.join("é.txt"), directory.join("á.txt")]
      normalized_files = [directory.join("é.txt"), directory.join("á.txt")]
      files.each { |file| file.write("content") }
      skip "The filesystem does not preserve distinct Unicode names" if normalized_files.any?(&:exist?)

      yield directory, files, normalized_files
    end
  end

  def run_finalization_failure(directory, files, normalized_files)
    staging_paths = normalized_files.map { |file| "gs://bucket/.task-googlecloud-staging/token/#{file.basename}" }
    final_paths = normalized_files.map { |file| "gs://bucket/#{file.basename}" }
    generations = expected_generations(staging_paths, final_paths)
    commands = []
    failure_command = Cloud::ObjectMove.command(staging_paths.last, final_paths.last)
    assert_raises(StandardError) do
      run_upload(directory, files, commands, failure_command, generations)
    end
    [commands, generations, staging_paths, final_paths]
  end

  def expected_generations(staging_paths, final_paths)
    [
      [staging_paths.first, "101"],
      [staging_paths.last, "102"],
      [final_paths.first, "201"],
      [staging_paths.first, "301"],
    ]
  end

  def run_upload(directory, files, commands, failure_command, generations)
    uploader = Cloud::Upload.new("project")
    exec =
      lambda do |command|
        commands << command
        raise StandardError, "finalization failed" if command == failure_command
      end

    uploader.stub(:upload_files_by_directory, { directory => files }) do
      stub_remote(exec, generations) { uploader.call }
    end
  end

  def stub_remote(exec, generations, &)
    Cloud.stub(:login, nil) do
      SecureRandom.stub(:hex, "token") do
        Cloud.stub(:pipe, generation_pipe(generations)) do
          Cloud.stub(:exec, exec, &)
        end
      end
    end
  end

  def generation_pipe(generations)
    lambda do |command, &block|
      path, generation = generations.shift
      assert_equal Shellwords.join(["gsutil", "stat", path]), command
      block.call(StringIO.new("Generation: #{generation}\n"))
    end
  end

  def expected_commands(normalized_files, staging_paths, final_paths)
    [Shellwords.join(%w[gcloud config set project project])] + copy_commands(normalized_files, staging_paths) +
      move_commands(staging_paths, final_paths) + rollback_commands(staging_paths, final_paths)
  end

  def copy_commands(normalized_files, staging_paths)
    normalized_files.zip(staging_paths).map do |normalized_file, staging_path|
      Shellwords.join(["gsutil", "cp", normalized_file.to_path, staging_path])
    end
  end

  def move_commands(staging_paths, final_paths)
    staging_paths.zip(final_paths).map do |staging_path, final_path|
      Cloud::ObjectMove.command(staging_path, final_path)
    end
  end

  def rollback_commands(staging_paths, final_paths)
    [
      Cloud::ObjectMove.rollback_command(staging_paths.first, final_paths.first, "201"),
      Cloud::ObjectMove.cleanup_command(staging_paths.first, "301"),
      Cloud::ObjectMove.cleanup_command(staging_paths.last, "102"),
    ]
  end
end
