require "minitest/autorun"
require "tmpdir"
require_relative "../lib/pathname_normalization"

class PathnameNormalizationTest < Minitest::Test
  # Ensure collision-free paths are renamed without changing their contents.
  def test_normalize_all_renames_every_path_after_validation
    Dir.mktmpdir do |directory|
      source = Pathname.new(directory).join("e\u0301.txt")
      source.write("content")

      normalized = Pathname.normalize_all([source])

      assert_equal [Pathname.new(directory).join("é.txt")], normalized
      assert_equal "content", normalized.first.read
    end
  end

  # Ensure preflight validation preserves both files when their normalized names collide.
  def test_normalize_all_does_not_rename_any_path_when_names_collide
    with_colliding_paths do |decomposed, composed|
      assert_raises(NormalizationPlan::CollisionError) do
        Pathname.normalize_all([decomposed, composed])
      end

      assert_equal "decomposed", decomposed.read
      assert_equal "composed", composed.read
    end
  end

  def test_apply_normalization_restores_paths_when_a_later_rename_fails
    Dir.mktmpdir do |directory|
      source, target, missing_source, missing_target = failure_paths(directory)
      source.write("content")

      assert_raises(Errno::ENOENT) do
        Pathname.apply_normalization(failure_entries(source, target, missing_source, missing_target))
      end

      assert_equal "content", source.read
      refute target.exist?
    end
  end

  private

  def failure_paths(directory)
    [
      Pathname.new(directory).join("first.txt"),
      Pathname.new(directory).join("normalized.txt"),
      Pathname.new(directory).join("missing.txt"),
      Pathname.new(directory).join("missing-normalized.txt"),
    ]
  end

  def failure_entries(source, target, missing_source, missing_target)
    [
      [source.to_path, target.to_path],
      [missing_source.to_path, missing_target.to_path],
    ]
  end

  # Create both Unicode forms and skip when the filesystem collapses them.
  def with_colliding_paths
    Dir.mktmpdir do |directory|
      decomposed = Pathname.new(directory).join("e\u0301.txt")
      composed = Pathname.new(directory).join("é.txt")
      decomposed.write("decomposed")
      composed.write("composed")
      skip_unless_distinct_unicode_names(directory)

      yield decomposed, composed
    end
  end

  # APFS and similar filesystems may store only one Unicode form for a name.
  def skip_unless_distinct_unicode_names(directory)
    return if Pathname.new(directory).children.size >= 2

    skip "The filesystem does not preserve distinct NFD and NFC names"
  end
end
