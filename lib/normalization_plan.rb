require_relative "string_normalization"

class NormalizationPlan
  class CollisionError < StandardError; end
  class RollbackError < StandardError; end

  # Surface cleanup failures without hiding the operation error that triggered rollback.
  def self.raise_on_rollback_failure(original_error, rollback_errors)
    return if rollback_errors.empty?

    details = rollback_errors.map(&:message).join("; ")
    raise RollbackError, "#{original_error.message}; rollback failed: #{details}"
  end

  # Build the complete plan and reject normalized-name collisions before callers make changes.
  def self.build(names)
    entries = names.map { |name| [name, name.normalized] }
    collisions = entries.group_by(&:last).select { |_name, sources| sources.size > 1 }

    unless collisions.empty?
      names = collisions.keys.join(", ")
      raise CollisionError, "Normalized names collide: #{names}"
    end

    entries
  end
end
