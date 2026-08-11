require "minitest/autorun"

class MiseTest < Minitest::Test
  def test_markdownlint_task_uses_the_app_image_outside_the_container
    task = markdownlint_task

    assert_includes task, "docker compose build app"
    assert_includes task, "docker compose run --rm --no-deps -e TASK_GOOGLECLOUD_IN_CONTAINER=1 app markdownlint-cli2"
    refute_includes task, "davidanson/markdownlint-cli2"
  end

  private

  def markdownlint_task
    File.read(File.expand_path("../mise.toml", __dir__))
        .split("[tasks.markdownlint]", 2).last
        .split("[tasks.test]", 2).first
  end
end
