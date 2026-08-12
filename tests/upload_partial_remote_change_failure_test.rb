require "minitest/autorun"
require "shellwords"
require "stringio"
require_relative "../lib/upload"

class UploadPartialRemoteChangeFailureTest < Minitest::Test
  STAGING_PATH = "gs://bucket/.task-googlecloud-staging/token/file.txt".freeze
  FINAL_PATH = "gs://bucket/file.txt".freeze
  private_constant :STAGING_PATH, :FINAL_PATH

  def test_upload_checks_the_staging_path_when_a_copy_receipt_is_missing
    commands = []

    with_probe(["present", "Generation: 303\n"], commands) { assert_staging_copy_requires_recovery }

    assert_equal expected_probe_commands, commands
  end

  def test_upload_requires_manual_recovery_when_an_object_remains_after_a_copy_error
    staged_files = []

    with_probe(["present", "Generation: 303\n"]) do
      Cloud::ObjectCopy.stub(:copy, ->(_source, _target) { raise copy_error }) do
        assert_raises(Cloud::ObjectMove::RecoveryRequiredError) { record_partial_upload(staged_files) }
      end
    end

    assert_empty staged_files
  end

  def test_upload_requires_manual_recovery_when_a_copy_error_initially_leaves_no_object
    staged_files = []

    with_probe(["missing"]) do
      Cloud::ObjectCopy.stub(:copy, ->(_source, _target) { raise copy_error }) do
        assert_raises(Cloud::ObjectMove::RecoveryRequiredError) { record_partial_upload(staged_files) }
      end
    end

    assert_empty staged_files
  end

  private

  def record_partial_upload(staged_files, target: STAGING_PATH)
    uploader = Cloud::Upload.new("project")
    uploader.__send__(:record_remote_change, STAGING_PATH, target, staged_files, remote_target: STAGING_PATH) do
      Cloud::ObjectCopy.copy("file.txt", STAGING_PATH)
    end
  end

  def assert_staging_copy_requires_recovery
    Cloud::ObjectCopy.stub(:copy, ->(_source, _target) { raise copy_error }) do
      assert_raises(Cloud::ObjectMove::RecoveryRequiredError) do
        record_partial_upload([], target: FINAL_PATH)
      end
    end
  end

  def expected_probe_commands
    [
      Cloud::ObjectMove.__send__(:state_command, STAGING_PATH),
      Shellwords.join(["gsutil", "stat", STAGING_PATH]),
    ]
  end

  def with_probe(responses, commands = [], &)
    pipe =
      lambda do |command, &pipe_block|
        commands << command
        pipe_block.call(StringIO.new(responses.shift))
      end
    Cloud.stub(:pipe, pipe, &)
    assert_empty responses
  end

  def copy_error
    Cloud::CommandError.new("Cloud.pipe", Cloud::ObjectCopy.command("file.txt", STAGING_PATH), nil)
  end
end
