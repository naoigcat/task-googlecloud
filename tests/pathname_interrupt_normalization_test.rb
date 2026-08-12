require "minitest/autorun"
require "tmpdir"
require_relative "../lib/pathname_normalization"

class PathnameInterruptNormalizationTest < Minitest::Test
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
    AtomicRename.stub(:rename, interrupting_rename(interrupt)) do
      Pathname.apply_normalization([[source.to_path, target.to_path]])
    end
  end

  def interrupting_rename(interrupt)
    original_rename = AtomicRename.method(:rename)
    lambda do |source, target|
      original_rename.call(source, target)
      interrupt.call
    end
  end

  def with_interrupt_after_side_effect
    release = Queue.new
    interrupter = interrupt_thread(release)
    yield interrupt_trigger(release)
  ensure
    release << true
    interrupter&.join
  end

  def interrupt_thread(release)
    Thread.new do
      release.pop
      Thread.main.raise(Interrupt)
    end
  end

  def interrupt_trigger(release)
    lambda do
      release << true
      Thread.pass
    end
  end
end
