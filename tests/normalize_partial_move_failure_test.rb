require "minitest/autorun"
require "stringio"
require_relative "../lib/normalize"

class NormalizePartialMoveFailureTest < Minitest::Test
  SOURCE = "gs://bucket/source.txt".freeze
  TEMPORARY = "gs://bucket/temporary.txt".freeze
  private_constant :SOURCE, :TEMPORARY

  def test_process_moves_requires_manual_recovery_when_only_a_destination_remains_after_a_move_error
    error, commands = run_failed_move(["missing", "present", "Generation: 202\n"], Cloud::ObjectMove::RecoveryRequiredError)

    assert_includes error.message, "Manual recovery required"
    assert_equal [Cloud::ObjectMove.command(SOURCE, TEMPORARY)], commands
  end

  def test_process_moves_requires_manual_recovery_when_both_paths_exist_after_a_move_error
    error, commands = run_failed_move(["present", "Generation: 101\n", "present", "Generation: 202\n"], Cloud::ObjectMove::RecoveryRequiredError)

    assert_includes error.message, "Manual recovery required"
    assert_equal [Cloud::ObjectMove.command(SOURCE, TEMPORARY)], commands
  end

  def test_process_moves_requires_manual_recovery_when_a_move_error_initially_leaves_no_change
    error, commands = run_failed_move(["present", "Generation: 101\n", "missing"], Cloud::ObjectMove::RecoveryRequiredError)

    assert_includes error.message, "Manual recovery required"
    assert_equal [Cloud::ObjectMove.command(SOURCE, TEMPORARY)], commands
  end

  def test_process_moves_confirms_after_a_read_error
    normalizer = Cloud::Normalize.new("project", "bucket")
    read_error = IOError.new("read failed")
    confirmations = []
    error = assert_read_error_confirmed(normalizer, read_error, confirmations)

    assert_same read_error, error

    assert_equal [[SOURCE, TEMPORARY, read_error]], confirmations
  end

  private

  def run_failed_move(responses, error_class)
    commands = []
    error =
      normalizer_with_failed_move(commands) do |normalizer|
        with_responses(responses, commands) do
          assert_raises(error_class) { normalizer.__send__(:process_moves, [[SOURCE, "target", TEMPORARY]], [], []) }
        end
      end
    assert_empty responses
    [error, commands]
  end

  def normalizer_with_failed_move(commands, &block)
    normalizer = Cloud::Normalize.new("project", "bucket")
    normalizer.stub(:move_object, failing_move(commands)) { block.call(normalizer) }
  end

  def with_responses(responses, commands, &block)
    Cloud.stub(:pipe, ->(_command, &pipe_block) { pipe_block.call(StringIO.new(responses.shift)) }) do
      Cloud.stub(:exec, recording_exec(commands)) { block.call }
    end
  end

  def failing_move(commands)
    lambda do |source, target|
      commands << Cloud::ObjectMove.command(source, target)
      raise Cloud::CommandError.new("Cloud.exec", Cloud::ObjectMove.command(SOURCE, TEMPORARY), nil)
    end
  end

  def recording_exec(commands)
    lambda do |command|
      commands << command
      nil
    end
  end

  def assert_read_error_confirmed(normalizer, read_error, confirmations)
    Cloud::ObjectMove.stub(:confirm_move_after_failure, ->(*args) { confirmations << args }) do
      normalizer.stub(:move_object, ->(_source, _target) { raise read_error }) do
        assert_raises(IOError) do
          normalizer.__send__(:process_moves, [[SOURCE, "target", TEMPORARY]], [], [])
        end
      end
    end
  end
end
