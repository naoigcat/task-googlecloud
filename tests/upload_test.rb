require "minitest/autorun"
require "pathname"
require "securerandom"
require "shellwords"
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
          Cloud.stub(:exec, ->(command) { commands << command }) { uploader.call }
        end
      end
    end
    commands
  end

  def expected_commands(project, directory, file)
    staging_path = "gs://#{directory.basename}/.task-googlecloud-staging/token/#{file.basename}"
    final_path = "gs://#{directory.basename}/#{file.basename}"
    [
      Shellwords.join(["gcloud", "config", "set", "project", project]),
      Shellwords.join(["gsutil", "cp", file.to_path, staging_path]),
      Cloud::ObjectMove.command(staging_path, final_path),
    ]
  end
end
