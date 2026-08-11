require "minitest/autorun"
require "tmpdir"
require_relative "../lib/pathname_normalization"
require_relative "interrupt_test_helper"

class PathnameInterruptNormalizationTest < Minitest::Test
  include InterruptTestHelper

  def test_apply_normalization_restores_paths_when_interrupted_after_the_rename
    Dir.mktmpdir do |directory|
      source, target = interrupted_paths(directory)

      with_interrupt_after_side_effect do |interrupt|
        assert_raises(Interrupt) { apply_interrupted_normalization(source, target, interrupt) }
      end

      assert_equal "content", source.read
      refute target.exist?
    end
  end

  private

  def interrupted_paths(directory)
    source = Pathname.new(directory).join("source.txt")
    target = Pathname.new(directory).join("normalized.txt")
    source.write("content")
    [source, target]
  end

  def apply_interrupted_normalization(source, target, interrupt)
    Pathname.stub(:new, interrupting_constructor(source, interrupt)) do
      Pathname.apply_normalization([[source.to_path, target.to_path]])
    end
  end

  def interrupting_constructor(source, interrupt)
    original_new = Pathname.method(:new)
    source_operation = interrupting_source_operation(source, interrupt)
    constructor_for_path(original_new, source, source_operation)
  end

  def constructor_for_path(original_new, source, source_operation)
    used_for_rename = false
    lambda do |path|
      if path == source.to_path && !used_for_rename
        used_for_rename = true
        source_operation
      else
        original_new.call(path)
      end
    end
  end

  def interrupting_source_operation(source, interrupt)
    operation = Object.new
    operation.define_singleton_method(:rename) do |target_path|
      source.rename(target_path)
      interrupt.call
    end
    operation
  end
end
