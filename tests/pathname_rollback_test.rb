require "minitest/autorun"
require "pathname"
require "tmpdir"
require_relative "../lib/pathname_normalization"

class PathnameRollbackTest < Minitest::Test
  def test_rollback_does_not_overwrite_an_existing_source
    Dir.mktmpdir do |directory|
      source = Pathname.new(directory).join("source.txt")
      target = Pathname.new(directory).join("target.txt")
      source.write("original")
      target.write("target")

      errors = Pathname.rollback_normalization([[source.to_path, target.to_path]])

      assert_instance_of Errno::EEXIST, errors.first
      assert_equal "original", source.read
      assert_equal "target", target.read
    end
  end

  def test_rollback_does_not_overwrite_a_source_created_after_preflight
    Dir.mktmpdir do |directory|
      source = Pathname.new(directory).join("source.txt")
      target = Pathname.new(directory).join("target.txt")
      target.write("target")
      rename = source_appearing_rename(source)

      assert_rollback_rejects_source(source, target, rename)
    end
  end

  def test_rollback_reports_when_both_paths_are_missing
    Dir.mktmpdir do |directory|
      source = Pathname.new(directory).join("source.txt")
      target = Pathname.new(directory).join("target.txt")

      errors = Pathname.rollback_normalization([[source.to_path, target.to_path]])

      assert_instance_of Errno::ENOENT, errors.first
    end
  end

  private

  def source_appearing_rename(source)
    original_rename = AtomicRename.method(:rename)
    lambda do |source_path, target_path|
      source.write("competitor") if target_path == source.to_path && !source.exist?
      original_rename.call(source_path, target_path)
    end
  end

  def assert_rollback_rejects_source(source, target, rename)
    errors =
      AtomicRename.stub(:rename, rename) do
        Pathname.rollback_normalization([[source.to_path, target.to_path]])
      end
    assert_instance_of Errno::EEXIST, errors.first
    assert_equal "competitor", source.read
    assert_equal "target", target.read
  end
end
