require "minitest/autorun"
require_relative "../lib/normalization_plan"
require_relative "../lib/string_normalization"

class NormalizationPlanTest < Minitest::Test
  # Ensure each plan entry retains its source name alongside the normalized target.
  def test_build_returns_normalized_names
    decomposed = "e\u0301.txt"

    assert_equal [[decomposed, "é.txt"]], NormalizationPlan.build([decomposed])
  end

  # Ensure a plan cannot proceed when distinct Unicode forms resolve to the same destination.
  def test_build_rejects_names_that_normalize_to_the_same_value
    decomposed = "e\u0301.txt"

    assert_raises(NormalizationPlan::CollisionError) do
      NormalizationPlan.build([decomposed, "é.txt"])
    end
  end
end
