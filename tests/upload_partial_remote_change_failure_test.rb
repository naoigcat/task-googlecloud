require "minitest/autorun"
require "stringio"
require_relative "../lib/upload"

class UploadPartialRemoteChangeFailureTest < Minitest::Test
  STAGING_PATH = "gs://bucket/.task-googlecloud-staging/token/file.txt".freeze
  private_constant :STAGING_PATH

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

  def record_partial_upload(staged_files)
    uploader = Cloud::Upload.new("project")
    uploader.__send__(:record_remote_change, STAGING_PATH, STAGING_PATH, staged_files) do
      Cloud::ObjectCopy.copy("file.txt", STAGING_PATH)
    end
  end

  def with_probe(responses, &block)
    Cloud.stub(:pipe, ->(_command, &pipe_block) { pipe_block.call(StringIO.new(responses.shift)) }) { block.call }
    assert_empty responses
  end

  def copy_error
    Cloud::CommandError.new("Cloud.pipe", Cloud::ObjectCopy.command("file.txt", STAGING_PATH), nil)
  end
end
