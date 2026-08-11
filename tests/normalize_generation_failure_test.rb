require "minitest/autorun"
require "stringio"
require_relative "../lib/normalize"

class NormalizeGenerationFailureTest < Minitest::Test
  SOURCE = "gs://bucket/source.txt".freeze
  TEMPORARY = "gs://bucket/temporary.txt".freeze
  private_constant :SOURCE, :TEMPORARY

  def test_process_moves_requires_manual_recovery_when_a_move_receipt_is_missing
    assert_includes run_missing_receipt.message, "Manual recovery required"
  end

  private

  def run_missing_receipt
    normalizer = Cloud::Normalize.new("project", "bucket")
    receipt_error = Cloud::ObjectMove::MissingGenerationError.new(TEMPORARY)

    normalizer.stub(:move_object, ->(_source, _target) { raise receipt_error }) do
      verify_partial_move(normalizer)
    end
  end

  def verify_partial_move(normalizer)
    responses = ["missing", "present", "Generation: 202\n"]
    error = Cloud.stub(:pipe, response_pipe(responses)) { assert_partial_move(normalizer) }
    assert_empty responses
    error
  end

  def assert_partial_move(normalizer)
    assert_raises(Cloud::ObjectMove::RecoveryRequiredError) do
      normalizer.__send__(:process_moves, [[SOURCE, "target", TEMPORARY]], [], [])
    end
  end

  def response_pipe(responses)
    ->(_command, &block) { block.call(StringIO.new(responses.shift)) }
  end
end
