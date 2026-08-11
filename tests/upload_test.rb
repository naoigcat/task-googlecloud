require "minitest/autorun"
require "pathname"
require "securerandom"
require "shellwords"
require "tmpdir"
require_relative "../lib/upload"

class UploadTest < Minitest::Test
  # Ensure project, bucket, and file names remain single remote-shell arguments.
  def test_call_escapes_arguments_in_remote_commands
    project = "project;$(touch pwned)"
    directory = Pathname.new("bucket;$(touch pwned)")
    file = directory.join("file;$(touch pwned) \"quoted\"")
    commands = run_upload(project, directory, file)

    assert_equal expected_commands(project, directory, file), commands
  end

  private

  def run_upload(project, directory, file)
    commands = []
    uploader = Cloud::Upload.new(project)
    uploader.stub(:upload_files_by_directory, { directory => [file] }) do
      Cloud.stub(:login, nil) do
        SecureRandom.stub(:hex, "token") do
          stub_remote_writes(commands) { uploader.call }
        end
      end
    end
    commands
  end

  def stub_remote_writes(commands, &)
    Cloud::ObjectCopy.stub(:copy, recording_copy(commands)) do
      Cloud::ObjectMove.stub(:move, recording_move(commands)) do
        Cloud.stub(:exec, recording_exec(commands), &)
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

  def recording_exec(commands)
    lambda do |command|
      commands << command
      nil
    end
  end

  def expected_commands(project, directory, file)
    staging_path = "gs://#{directory.basename}/.task-googlecloud-staging/token/#{file.basename}"
    final_path = "gs://#{directory.basename}/#{file.basename}"
    [
      Shellwords.join(["gcloud", "config", "set", "project", project]),
      Cloud::ObjectCopy.command(file.to_path, staging_path),
      Cloud::ObjectMove.command("#{staging_path}#101", final_path, source_path: staging_path),
    ]
  end
end
