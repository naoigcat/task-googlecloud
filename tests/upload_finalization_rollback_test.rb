require "minitest/autorun"
require "pathname"
require "securerandom"
require "shellwords"
require "tmpdir"
require_relative "../lib/upload"

class UploadFinalizationRollbackTest < Minitest::Test
  def test_call_cleans_recreated_staging_generation_when_later_finalization_fails
    with_two_unicode_upload do |directory, files, normalized_files|
      commands = []

      assert_raises(StandardError) { run_upload(directory, files, commands) }
      assert_equal expected_commands(normalized_files), commands
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

  def run_upload(directory, files, commands)
    uploader = Cloud::Upload.new("project")
    uploader.stub(:upload_files_by_directory, { directory => files }) do
      Cloud.stub(:login, nil) do
        SecureRandom.stub(:hex, "token") do
          stub_remote(commands) { uploader.call }
        end
      end
    end
  end

  def stub_remote(commands, &)
    Cloud::ObjectCopy.stub(:copy, recording_copy(commands)) do
      Cloud::ObjectMove.stub(:move, failing_second_move(commands)) do
        Cloud::ObjectMove.stub(:rollback, recording_rollback(commands)) do
          Cloud.stub(:exec, recording_exec(commands), &)
        end
      end
    end
  end

  def recording_copy(commands)
    generations = %w[101 102]
    lambda do |source, target|
      commands << Cloud::ObjectCopy.command(source, target)
      generations.shift
    end
  end

  def failing_second_move(commands)
    moves = 0
    lambda do |source, target|
      moves += 1
      commands << Cloud::ObjectMove.command("#{source}#10#{moves}", target, source_path: source)
      raise StandardError, "finalization failed" if moves == 2

      "201"
    end
  end

  def recording_rollback(commands)
    lambda do |source, target, generation|
      commands << Cloud::ObjectMove.rollback_command(source, target, generation)
      "301"
    end
  end

  def recording_exec(commands)
    lambda do |command|
      commands << command
      nil
    end
  end

  def expected_commands(normalized_files)
    staging_paths = normalized_files.map { |file| staging_path(file) }
    final_paths = normalized_files.map { |file| final_path(file) }
    [Shellwords.join(%w[gcloud config set project project])] + copy_commands(normalized_files, staging_paths) +
      move_commands(staging_paths, final_paths) + rollback_commands(staging_paths, final_paths)
  end

  def staging_path(file)
    "gs://bucket/.task-googlecloud-staging/token/#{file.basename}"
  end

  def final_path(file)
    "gs://bucket/#{file.basename}"
  end

  def copy_commands(normalized_files, staging_paths)
    normalized_files.zip(staging_paths).map { |file, staging| Cloud::ObjectCopy.command(file.to_path, staging) }
  end

  def move_commands(staging_paths, final_paths)
    staging_paths.zip(final_paths).each_with_index.map do |(staging, final_path), index|
      Cloud::ObjectMove.command("#{staging}#10#{index + 1}", final_path, source_path: staging)
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
