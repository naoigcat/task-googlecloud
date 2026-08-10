require "minitest/autorun"
require "shellwords"
require "stringio"
require_relative "../lib/cloud"
require_relative "../lib/cloud_move"

class CloudMoveTest < Minitest::Test
  def test_generation_reads_the_target_object_generation
    Cloud.stub(:pipe, ->(_command, &block) { block.call(StringIO.new("Generation: 123\n")) }) do
      assert_equal "123", Cloud::ObjectMove.generation("gs://bucket/file.txt")
    end
  end

  def test_rollback_uses_the_recorded_generation
    command = Cloud::ObjectMove.rollback_command("gs://bucket/staged.txt", "gs://bucket/file.txt", "123")

    assert_includes command, Shellwords.join(["gsutil", "-q", "stat", "gs://bucket/file.txt#123"])
    assert_includes(
      command,
      Shellwords.join(
        ["gsutil", "-h", "x-goog-if-generation-match:0", "cp", "gs://bucket/file.txt#123", "gs://bucket/staged.txt"],
      ),
    )
    assert_includes command, Shellwords.join(["gsutil", "rm", "-f", "gs://bucket/file.txt#123"])
    assert_includes command, "&& ! #{Shellwords.join(["gsutil", "-q", "stat", "gs://bucket/file.txt"])}"
  end

  def test_cleanup_uses_the_recorded_generation
    command = Cloud::ObjectMove.cleanup_command("gs://bucket/staged.txt", "123")

    assert_includes command, Shellwords.join(["gsutil", "-q", "stat", "gs://bucket/staged.txt#123"])
    assert_includes command, Shellwords.join(["gsutil", "rm", "-f", "gs://bucket/staged.txt#123"])
  end
end
