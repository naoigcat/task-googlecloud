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

  def test_move_records_the_generation_emitted_by_the_move
    source = "gs://bucket/source.txt"
    target = "gs://bucket/target.txt"
    commands = []
    responses = ["Generation: 123\n", "Created: #{target}#456\n"]

    Cloud.stub(:pipe, recording_pipe(commands, responses)) do
      assert_equal "456", Cloud::ObjectMove.move(source, target)
    end

    assert_empty responses
    assert_equal [Shellwords.join(["gsutil", "stat", source]), move_command(source, target)], commands
  end

  def test_copy_requires_an_absent_target_and_records_its_emitted_generation
    source = "/tmp/file.txt"
    target = "gs://bucket/target.txt"
    commands = []

    Cloud.stub(:pipe, recording_pipe(commands, ["Created: #{target}#456\n"])) do
      assert_equal "456", Cloud::ObjectCopy.copy(source, target)
    end

    assert_equal [Cloud::ObjectCopy.command(source, target)], commands
    assert_includes commands.first, "x-goog-if-generation-match:0"
  end

  def test_copy_rejects_a_receipt_for_a_different_target
    error =
      assert_raises(Cloud::ObjectMove::MissingGenerationError) do
        Cloud::ObjectCopy.receipt("Created: gs://bucket/other.txt#456\n", "gs://bucket/target.txt")
      end

    assert_includes error.message, "target.txt"
  end

  def test_rollback_uses_the_recorded_generation
    command = Cloud::ObjectMove.rollback_command("gs://bucket/staged.txt", "gs://bucket/file.txt", "123")

    assert_uses_state_command(command, "gs://bucket/file.txt#123")
    assert_copies_exact_generation(command)
    assert_includes command, Shellwords.join(["gsutil", "rm", "gs://bucket/file.txt#123"])
    assert_includes command, "[ \"$target_state\" = missing ] || exit 1"
    refute_includes command, "then exit 0"
    refute_includes command, "! gsutil"
  end

  def test_rollback_records_the_generation_emitted_by_the_restore
    source = "gs://bucket/staged.txt"
    target = "gs://bucket/file.txt"
    commands = []

    Cloud.stub(:pipe, recording_pipe(commands, ["Created: #{source}#456\n"])) do
      assert_equal "456", Cloud::ObjectMove.rollback(source, target, "123")
    end

    assert_equal [Cloud::ObjectMove.rollback_command(source, target, "123")], commands
  end

  def test_cleanup_uses_the_recorded_generation
    command = Cloud::ObjectMove.cleanup_command("gs://bucket/staged.txt", "123")

    assert_uses_state_command(command, "gs://bucket/staged.txt#123")
    assert_includes command, Shellwords.join(["gsutil", "rm", "gs://bucket/staged.txt#123"])
    refute_includes command, "! gsutil"
  end

  def test_confirmation_requires_manual_recovery_when_the_state_probe_fails
    operation_error = Cloud::CommandError.new("Cloud.exec", "gsutil mv source target", nil)
    confirmation_error = Cloud::CommandError.new("Cloud.pipe", "gsutil stat source", nil)

    error =
      Cloud.stub(:pipe, ->(_command, &) { raise confirmation_error }) do
        assert_raises(Cloud::ObjectMove::RecoveryRequiredError) do
          Cloud::ObjectMove.confirm_move_after_failure("gs://bucket/source", "gs://bucket/target", operation_error)
        end
      end

    assert_includes error.message, "Manual recovery required"
    assert_includes error.message, "state confirmation failed"
  end

  private

  def recording_pipe(commands, responses)
    lambda do |command, &block|
      commands << command
      block.call(StringIO.new(responses.shift))
    end
  end

  def move_command(source, target)
    Cloud::ObjectMove.command(source, target, source_path: source, source_generation: "123")
  end

  def assert_uses_state_command(command, path)
    assert_includes command, Shellwords.join(["env", "LC_ALL=C", "gsutil", "stat", path])
  end

  def assert_copies_exact_generation(command)
    copy = Shellwords.join(
      %w[gsutil -h x-goog-if-generation-match:0 cp -v] + %w[gs://bucket/file.txt#123 gs://bucket/staged.txt],
    )

    assert_includes command, copy
  end
end
