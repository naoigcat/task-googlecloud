require "minitest/autorun"
require "shellwords"
require_relative "../lib/normalize"
require_relative "interrupt_test_helper"

class NormalizeInterruptTransactionTest < Minitest::Test
  include InterruptTestHelper

  def test_call_restores_staged_objects_when_interrupted_after_the_move
    commands = run_interrupted_normalization([%w[source target temporary]], "temporary")

    assert_equal(
      [
        Cloud::ObjectMove.command("source", "temporary"),
        Cloud::ObjectMove.rollback_command("source", "temporary", "101"),
      ],
      commands,
    )
  end

  def test_call_restores_finalized_objects_when_interrupted_after_the_move
    moves = [%w[source target temporary], %w[other normalized final-temporary]]
    commands = run_interrupted_normalization(moves, "normalized")

    assert_equal expected_finalization_commands, commands
  end

  private

  def run_interrupted_normalization(moves, interrupted_target)
    commands = []
    with_interrupt_after_side_effect do |interrupt|
      normalizer = Cloud::Normalize.new("project", "bucket")
      stub_interrupted_normalizer(normalizer, moves, interrupted_target, commands, interrupt)
    end
    commands
  end

  def stub_interrupted_normalizer(normalizer, moves, interrupted_target, commands, interrupt)
    normalizer.stub(:move_object, interrupted_move(commands, interrupted_target, interrupt)) do
      Cloud::ObjectMove.stub(:generation, "101") do
        Cloud.stub(:exec, recording_exec(commands)) do
          assert_raises(Interrupt) { normalizer.__send__(:process_moves, moves, [], []) }
        end
      end
    end
  end

  def interrupted_move(commands, interrupted_target, interrupt)
    lambda do |source, target|
      commands << Cloud::ObjectMove.command(source, target)
      interrupt.call if target == interrupted_target
    end
  end

  def recording_exec(commands)
    lambda do |command|
      commands << command
      nil
    end
  end

  def expected_finalization_commands
    [
      Cloud::ObjectMove.command("source", "temporary"),
      Cloud::ObjectMove.command("other", "final-temporary"),
      Cloud::ObjectMove.command("temporary", "target"),
      Cloud::ObjectMove.command("final-temporary", "normalized"),
      Cloud::ObjectMove.rollback_command("other", "normalized", "101"),
      Cloud::ObjectMove.rollback_command("source", "target", "101"),
    ]
  end
end
