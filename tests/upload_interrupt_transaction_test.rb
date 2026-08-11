require "minitest/autorun"
require "pathname"
require "securerandom"
require "shellwords"
require "tmpdir"
require_relative "../lib/upload"
require_relative "interrupt_test_helper"

class UploadInterruptTransactionTest < Minitest::Test
  include InterruptTestHelper

  def test_call_restores_remote_and_local_paths_when_interrupted_after_finalization
    with_unicode_upload do |directory, file, normalized_file|
      commands = run_interrupted_upload(directory, file, :finalization)

      assert_equal "content", file.read
      refute normalized_file.exist?
      assert_equal expected_finalization_commands(file), commands
    end
  end

  def test_call_restores_staged_upload_when_interrupted_after_the_copy
    with_unicode_upload do |directory, file, normalized_file|
      commands = run_interrupted_upload(directory, file, :staging)

      assert_equal "content", file.read
      refute normalized_file.exist?
      assert_equal expected_staging_commands(file), commands
    end
  end

  private

  def run_interrupted_upload(directory, file, phase)
    commands = []
    with_interrupt_after_side_effect do |trigger|
      run_upload_with_interrupt(directory, file, phase, commands, trigger)
    end
    commands
  end

  def run_upload_with_interrupt(directory, file, phase, commands, trigger)
    uploader = Cloud::Upload.new("project")
    uploader.stub(:upload_files_by_directory, { directory => [file] }) do
      Cloud.stub(:login, nil) do
        SecureRandom.stub(:hex, "token") do
          stub_remote(phase, commands, trigger) { assert_raises(Interrupt) { uploader.call } }
        end
      end
    end
  end

  def stub_remote(phase, commands, trigger, &)
    Cloud::ObjectCopy.stub(:copy, interrupted_copy(phase, commands, trigger)) do
      Cloud::ObjectMove.stub(:move, interrupted_move(phase, commands, trigger)) do
        Cloud::ObjectMove.stub(:rollback, interrupted_rollback(commands)) do
          Cloud.stub(:exec, recording_exec(commands), &)
        end
      end
    end
  end

  def interrupted_copy(phase, commands, trigger)
    lambda do |source, target|
      commands << Cloud::ObjectCopy.command(source, target)
      trigger.call if phase == :staging
      "101"
    end
  end

  def interrupted_move(phase, commands, trigger)
    lambda do |source, target|
      commands << Cloud::ObjectMove.command("#{source}#101", target, source_path: source)
      trigger.call if phase == :finalization
      "102"
    end
  end

  def interrupted_rollback(commands)
    lambda do |source, target, generation|
      commands << Cloud::ObjectMove.rollback_command(source, target, generation)
      "103"
    end
  end

  def recording_exec(commands)
    lambda do |command|
      commands << command
      nil
    end
  end

  def staging_path(file) = "gs://bucket/.task-googlecloud-staging/token/#{normalized_basename(file)}"

  def final_path(file) = "gs://bucket/#{normalized_basename(file)}"

  def expected_commands(file)
    [
      Shellwords.join(%w[gcloud config set project project]),
      Cloud::ObjectCopy.command(file.dirname.join(normalized_basename(file)).to_path, staging_path(file)),
      Cloud::ObjectMove.command("#{staging_path(file)}#101", final_path(file), source_path: staging_path(file)),
    ]
  end

  def normalized_basename(file) = file.basename.to_s.normalized

  def expected_finalization_commands(file)
    expected_commands(file) + [
      Cloud::ObjectMove.rollback_command(staging_path(file), final_path(file), "102"),
      Cloud::ObjectMove.cleanup_command(staging_path(file), "103"),
    ]
  end

  def expected_staging_commands(file)
    expected_commands(file).first(2) + [Cloud::ObjectMove.cleanup_command(staging_path(file), "101")]
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
end
