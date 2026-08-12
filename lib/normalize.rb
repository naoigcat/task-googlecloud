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
      # The API returns object names as data, so wildcard characters cannot alter the listing.
      files = Cloud.pipe(Cloud::StorageApi.list_command(@bucket), &:readlines).map(&:chomp)

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
    rescue StandardError, SignalException => e
      rollback_after_failure(e, staged, finalized)
    end

    def rollback_after_failure(error, staged, finalized)
      rollback_errors = rollback_objects(staged, finalized)
      NormalizationPlan.raise_on_rollback_failure(error, rollback_errors)
      raise error
    end

    def stage_objects(moves, staged)
      moves.each do |source, _target, temporary|
        record_move(source, temporary, staged)
      end
    end

    def finalize_objects(moves, finalized)
      moves.each do |source, target, temporary|
        record_move(temporary, target, finalized) { |move| move[0] = source }
      end
    end

    def record_move(source, target, moves)
      # Defer signals until the move and generation are recorded so rollback sees every side effect.
      Thread.handle_interrupt(SignalException => :never) do
        execute_and_record_move(source, target, moves) { |move| yield move if block_given? }
      end
    end

    def execute_and_record_move(source, target, moves)
      target_generation = move_object(source, target)
      moves << [source, target, target_generation]
      yield moves.last
    rescue Cloud::CommandError, Cloud::ObjectMove::MissingGenerationError, IOError, SystemCallError => e
      Cloud::ObjectMove.confirm_move_after_failure(source, target, e) unless target_generation
      raise
    end

    def temporary_path(source)
      "#{source}.task-googlecloud-#{SecureRandom.hex(16)}"
    end

    def move_object(source, target)
      target_generation = Cloud::ObjectMove.move(source, target)
      sleep 1
      target_generation
    end

    def rollback_objects(staged, finalized)
      rollback_finalized(finalized) + rollback_staged(staged, finalized)
    end

    def rollback_finalized(finalized)
      finalized.reverse_each.filter_map do |temporary, target, target_generation|
        attempt_rollback(temporary, target, target_generation)
      end
    end

    def rollback_staged(staged, finalized)
      finalized_sources = finalized.map(&:first)
      staged.reverse_each.filter_map do |source, temporary, temporary_generation|
        # Finalization already restored this source; replaying staging would use an obsolete generation.
        next if finalized_sources.include?(source)

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
