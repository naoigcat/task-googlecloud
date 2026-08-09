require "minitest/autorun"
require "yaml"

class WorkflowTest < Minitest::Test
  def setup
    @workflow = File.read(File.expand_path("../.github/workflows/lint.yml", __dir__))
    @config = YAML.safe_load(@workflow)
  end

  def test_ci_runs_the_test_task_for_pushes_and_pull_requests
    assert_triggers

    test_job = @config["jobs"]["test"]
    assert_equal "ubuntu-latest", test_job["runs-on"]
    assert_test_steps(test_job["steps"])
  end

  private

  def assert_triggers
    triggers = @config["on"] || @config[true]
    assert triggers.key?("push")
    assert triggers.key?("pull_request")
  end

  def assert_test_steps(steps)
    assert(steps.any? { |step| step["name"] == "Setup mise" && step["uses"] == "jdx/mise-action@v2" })
    assert(steps.any? { |step| step["name"] == "Ruby tests" && step["run"] == "mise run test" })
  end
end
