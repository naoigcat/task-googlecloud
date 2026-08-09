require "pathname"
require_relative "normalization_plan"
require_relative "string_normalization"

module PathnameNormalization
  # Validate every path before renaming to prevent partial changes caused by a collision.
  def normalize_all(paths)
    apply_normalization(normalization_plan(paths))
  end

  # Separate collision validation from file changes so callers can control when side effects occur.
  def normalization_plan(paths)
    NormalizationPlan.build(paths.map(&:to_path))
  end

  # Apply a prepared plan and restore completed renames if a later rename fails.
  def apply_normalization(entries)
    renamed = []
    apply_renames(entries, renamed)

    entries.map { |_source, target| Pathname.new(target) }
  rescue StandardError => e
    rollback_after_failure(e, renamed)
  end

  def apply_renames(entries, renamed)
    entries.each do |source, target|
      next if source == target

      Pathname.new(source).rename(target)
      renamed << [source, target]
    end
  end

  def rollback_after_failure(error, renamed)
    rollback_errors = rollback_normalization(renamed)
    NormalizationPlan.raise_on_rollback_failure(error, rollback_errors)
    raise error
  end

  # Restore normalized paths before callers expose a partially completed operation.
  def rollback_normalization(entries)
    errors = []
    entries.reverse_each do |source, target|
      next if source == target

      error = attempt_rollback(source, target)
      errors << error if error
    end
    errors
  end

  # Try every path so one cleanup failure does not block later restorations.
  def attempt_rollback(source, target)
    source_path = Pathname.new(source)
    target_path = Pathname.new(target)
    raise Errno::EEXIST if source_path.exist? && target_path.exist?
    raise Errno::ENOENT unless target_path.exist? && !source_path.exist?

    target_path.rename(source)
    nil
  rescue StandardError => e
    e
  end
end

# Expose rename planning on Pathname so upload and normalize share one API.
Pathname.extend(PathnameNormalization)
