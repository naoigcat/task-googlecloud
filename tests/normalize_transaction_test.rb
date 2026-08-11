require "minitest/autorun"
require "securerandom"
require "shellwords"
require "stringio"
require_relative "../lib/normalize"

class NormalizeTransactionTest < Minitest::Test
  def test_call_restores_staged_objects_when_staging_fails
    source_one = "gs://bucket/é.txt"
    source_two = "gs://bucket/á.txt"
    temporary_one = "#{source_one}.task-googlecloud-token"
    temporary_two = "#{source_two}.task-googlecloud-token"

    failure_command = Cloud::ObjectMove.command(source_two, temporary_two)
    assert_raises(StandardError) { run_failing_normalization(source_one, source_two, failure_command) }

    assert_equal expected_commands(source_one, source_two, temporary_one, temporary_two), @commands
  end

  def test_call_restores_staged_objects_when_interrupted
    source_one = "gs://bucket/é.txt"
    source_two = "gs://bucket/á.txt"
    temporary_one = "#{source_one}.task-googlecloud-token"
    temporary_two = "#{source_two}.task-googlecloud-token"

    failure_command = Cloud::ObjectMove.command(source_two, temporary_two)
    assert_raises(Interrupt) do
      run_failing_normalization(source_one, source_two, failure_command, Interrupt.new)
    end

    assert_equal expected_commands(source_one, source_two, temporary_one, temporary_two), @commands
  end

  def test_call_restores_staged_objects_when_finalization_fails
    sources = ["gs://bucket/é.txt", "gs://bucket/á.txt"]
    temporaries = sources.map { |source| "#{source}.task-googlecloud-token" }
    targets = sources.map(&:normalized)
    failure_command = Cloud::ObjectMove.command(temporaries[1], targets[1])

    assert_raises(StandardError) { run_failing_normalization(sources.first, sources.last, failure_command) }

    assert_equal expected_finalization_commands(sources, temporaries, targets), @commands
  end

  private

  def run_failing_normalization(source_one, source_two, failure_command, error = StandardError.new("staging failed"))
    @commands = []
    SecureRandom.stub(:hex, "token") do
      stub_cloud(@commands, source_one, source_two, failure_command, error) do
        Cloud::Normalize.call("project", "bucket")
      end
    end
  end

  def stub_cloud(commands, source_one, source_two, failure_command, error = StandardError.new("staging failed"), &)
    Cloud::Normalize.stub(:sleep, nil) do
      Cloud.stub(:login, nil) do
        Cloud.stub(:pipe, listing_pipe(source_one, source_two)) do
          Cloud.stub(:exec, failing_exec(commands, failure_command, error), &)
        end
      end
    end
  end

  def listing_pipe(source_one, source_two)
    lambda do |command, &block|
      if command.start_with?("gsutil stat ")
        block.call(StringIO.new("Generation: 101\n"))
      else
        block.call(StringIO.new("#{source_one}\n#{source_two}\n"))
      end
    end
  end

  def failing_exec(commands, failure_command, error)
    failed = false
    lambda do |command|
      commands << command
      next unless command == failure_command && !failed

      failed = true
      raise error
    end
  end

  def expected_commands(source_one, source_two, temporary_one, temporary_two)
    [
      Shellwords.join(%w[gcloud config set project project]),
      Cloud::ObjectMove.command(source_one, temporary_one),
      Cloud::ObjectMove.command(source_two, temporary_two),
      Cloud::ObjectMove.rollback_command(source_one, temporary_one, "101"),
    ]
  end

  def expected_finalization_commands(sources, temporaries, targets)
    commands = [Shellwords.join(%w[gcloud config set project project])]
    commands.concat(move_commands(sources, temporaries))
    commands.concat(move_commands(temporaries, targets))
    commands.concat(rollback_commands([temporaries.first], [targets.first]))
    commands.concat(rollback_commands(sources, temporaries))
    commands
  end

  def move_commands(sources, targets)
    sources.zip(targets).map { |source, target| Cloud::ObjectMove.command(source, target) }
  end

  def rollback_commands(sources, targets)
    sources.zip(targets).reverse_each.map do |source, target|
      Cloud::ObjectMove.rollback_command(source, target, "101")
    end
  end
end
