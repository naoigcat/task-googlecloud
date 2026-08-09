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

  def test_rollback_reports_when_both_paths_are_missing
    Dir.mktmpdir do |directory|
      source = Pathname.new(directory).join("source.txt")
      target = Pathname.new(directory).join("target.txt")

      errors = Pathname.rollback_normalization([[source.to_path, target.to_path]])

      assert_instance_of Errno::ENOENT, errors.first
    end
  end
end
