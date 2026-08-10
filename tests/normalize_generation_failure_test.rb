require "minitest/autorun"
require "securerandom"
require "shellwords"
require "stringio"
require_relative "../lib/normalize"

class NormalizeGenerationFailureTest < Minitest::Test
  def test_call_reports_manual_rollback_when_generation_lookup_fails_after_a_move
    error =
      assert_raises(NormalizationPlan::RollbackError) do
        run_generation_lookup_failure("gs://bucket/é.txt", "gs://bucket/á.txt")
      end

    assert_includes error.message, "Cannot verify ownership"
  end

  private

  def run_generation_lookup_failure(source_one, source_two)
    pipe = generation_failure_pipe(source_one, source_two)
    SecureRandom.stub(:hex, "token") do
      Cloud::Normalize.stub(:sleep, nil) do
        stub_cloud(pipe) { Cloud::Normalize.call("project", "bucket") }
      end
    end
  end

  def stub_cloud(pipe, &)
    Cloud.stub(:login, nil) do
      Cloud.stub(:pipe, pipe) do
        Cloud.stub(:exec, ->(_command) {}, &)
      end
    end
  end

  def generation_failure_pipe(source_one, source_two)
    lambda do |command, &block|
      raise Cloud::ObjectMove::MissingGenerationError, command if command.start_with?("gsutil stat ")

      block.call(StringIO.new("#{source_one}\n#{source_two}\n"))
    end
  end
end
