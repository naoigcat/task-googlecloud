require "securerandom"
require "shellwords"
require_relative "cloud"
require_relative "cloud_move"
require_relative "normalization_plan"

module Cloud
  class Normalize
    # Keep construction and execution together so CLI entry points do not duplicate the workflow.
    def self.call(project, bucket)
      new(project, bucket).call
    end

    # Keep the target fixed throughout the operation to avoid acting on a different bucket.
    def initialize(project, bucket)
      @project = project
      @bucket = bucket
    end

    # Validate names before staging objects so failures can restore the original layout.
    def call
      Cloud.login
      Cloud.exec(Shellwords.join(["gcloud", "config", "set", "project", @project]))
      files = Cloud.pipe(Shellwords.join(["gsutil", "ls", "gs://#{@bucket}/**"]), &:readlines).map(&:chomp)

      normalize_objects(NormalizationPlan.build(files).reject { |source, target| source == target })
    end

    private

    def normalize_objects(entries)
      moves = entries.map { |source, target| [source, target, temporary_path(source)] }
      staged = []
      finalized = []

      process_moves(moves, staged, finalized)
    end

    def process_moves(moves, staged, finalized)
      stage_objects(moves, staged)
      finalize_objects(moves, finalized)
    rescue StandardError => e
      rollback_after_failure(e, staged, finalized)
    end

    def rollback_after_failure(error, staged, finalized)
      rollback_errors = rollback_objects(staged, finalized)
      NormalizationPlan.raise_on_rollback_failure(error, rollback_errors)
      raise error
    end

    def stage_objects(moves, staged)
      moves.each do |source, _target, temporary|
        move_object(source, temporary)
        staged << [source, temporary, nil]
        staged.last[2] = Cloud::ObjectMove.generation(temporary)
      end
    end

    def finalize_objects(moves, finalized)
      moves.each do |_source, target, temporary|
        move_object(temporary, target)
        finalized << [temporary, target, nil]
        finalized.last[2] = Cloud::ObjectMove.generation(target)
      end
    end

    def temporary_path(source)
      "#{source}.task-googlecloud-#{SecureRandom.hex(16)}"
    end

    def move_object(source, target)
      Cloud.exec(Cloud::ObjectMove.command(source, target))
      sleep 1
    end

    def rollback_objects(staged, finalized)
      rollback_finalized(finalized) + rollback_staged(staged)
    end

    def rollback_finalized(finalized)
      finalized.reverse_each.filter_map do |temporary, target, target_generation|
        attempt_rollback(temporary, target, target_generation)
      end
    end

    def rollback_staged(staged)
      staged.reverse_each.filter_map do |source, temporary, temporary_generation|
        attempt_rollback(source, temporary, temporary_generation)
      end
    end

    # Try every known move so one cleanup failure does not block later restorations.
    def attempt_rollback(source, target, target_generation)
      Cloud.exec(Cloud::ObjectMove.rollback_command(source, target, target_generation))
    rescue StandardError => e
      e
    end
  end
end
