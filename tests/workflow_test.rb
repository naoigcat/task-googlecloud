require "minitest/autorun"
require "yaml"

class WorkflowTest < Minitest::Test
  CHECKOUT_ACTION = "actions/checkout@f43a0e5ff2bd294095638e18286ca9a3d1956744".freeze
  MISE_ACTION = "jdx/mise-action@c37c93293d6b742fc901e1406b8f764f6fb19dac".freeze
  private_constant :CHECKOUT_ACTION, :MISE_ACTION

  def setup
    @workflow = File.read(File.expand_path("../.github/workflows/lint-and-test.yml", __dir__))
    @config = YAML.safe_load(@workflow)
  end

  def test_ci_runs_the_test_task_for_pushes_and_pull_requests
    assert_equal "Lint and test", @config["name"]
    assert_triggers

    test_job = @config["jobs"]["test"]
    assert_equal "ubuntu-latest", test_job["runs-on"]
    assert_test_steps(test_job["steps"])
    assert_lint_steps(@config["jobs"]["lint"]["steps"])
  end

  private

  def assert_triggers
    triggers = @config["on"] || @config[true]
    assert triggers.key?("push")
    assert triggers.key?("pull_request")
  end

  def assert_test_steps(steps)
    assert_pinned_actions(steps)
    assert(steps.any? { |step| step["name"] == "Ruby tests" && step["run"] == "mise run test" })
  end

  def assert_lint_steps(steps)
    assert_pinned_actions(steps)
    assert(steps.any? { |step| step["name"] == "RuboCop" && step["run"] == "mise run rubocop" })
  end

  def assert_pinned_actions(steps)
    action_steps = steps.select { |step| ["Checkout", "Setup mise"].include?(step["name"]) }
    assert_equal(
      [
        ["Checkout", CHECKOUT_ACTION],
        ["Setup mise", MISE_ACTION],
      ],
      action_steps.map { |step| [step["name"], step["uses"]] },
    )
  end
end
