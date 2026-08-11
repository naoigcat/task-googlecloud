require "securerandom"
require "shellwords"
require_relative "cloud"
require_relative "cloud_move"

module Cloud
  class Upload
    # Keep construction and execution together so CLI entry points do not duplicate the workflow.
    def self.call(project)
      new(project).call
    end

    # Keep the project fixed on the instance so it cannot change during an upload.
    def initialize(project)
      @project = project
    end

    # Build the complete rename plan before authentication so a collision cannot cause partial changes.
    def call
      files_by_directory = upload_files_by_directory
      files = files_by_directory.values.flatten
      normalization_plan = Pathname.normalization_plan(files)

      authenticate_project
      normalized_files = files.zip(Pathname.apply_normalization(normalization_plan)).to_h
      upload_with_rollback(files_by_directory, normalized_files, normalization_plan)
    end

    private

    # Collect every upload path up front so collision detection can see the complete set.
    def upload_files_by_directory
      Pathname.new("../uploads").expand_path(__dir__).glob("*")
              .to_h { |directory| [directory, directory.glob("*")] }
    end

    # Authenticate only after the rename plan is known to be collision-free.
    def authenticate_project
      Cloud.login
      Cloud.exec(Shellwords.join(["gcloud", "config", "set", "project", @project]))
    end

    def upload_with_rollback(files_by_directory, normalized_files, normalization_plan)
      upload_normalized_files(files_by_directory, normalized_files)
    rescue StandardError, SignalException => e
      rollback_errors = Pathname.rollback_normalization(normalization_plan)
      NormalizationPlan.raise_on_rollback_failure(e, rollback_errors)
      raise
    end

    # Stage uploads before finalizing them so failed batches can remove remote changes.
    def upload_normalized_files(files_by_directory, normalized_files)
      staging_prefix = ".task-googlecloud-staging/#{SecureRandom.hex(16)}"
      staged_files = []
      finalized_files = []

      perform_upload(files_by_directory, normalized_files, staging_prefix, staged_files, finalized_files)
    rescue StandardError, SignalException => e
      rollback_errors = rollback_uploads(staged_files, finalized_files)
      NormalizationPlan.raise_on_rollback_failure(e, rollback_errors)
      raise
    end

    def perform_upload(files_by_directory, normalized_files, staging_prefix, staged_files, finalized_files)
      files_by_directory.each do |directory, directory_files|
        stage_directory(directory, directory_files, normalized_files, staging_prefix, staged_files)
      end

      finalize_uploads(staged_files, finalized_files)
    end

    def stage_directory(directory, directory_files, normalized_files, staging_prefix, staged_files)
      puts directory
      bucket = directory.basename.to_path
      directory_files.each do |file|
        normalized_file = normalized_files[file]
        staging_path = "gs://#{bucket}/#{staging_prefix}/#{normalized_file.basename}"
        final_path = "gs://#{bucket}/#{normalized_file.basename}"
        record_remote_change(staging_path, final_path, staging_path, staged_files) do
          Cloud.exec(Shellwords.join(["gsutil", "cp", normalized_file.to_path, staging_path]))
        end
      end
    end

    def finalize_uploads(staged_files, finalized_files)
      staged_files.each do |staging_path, final_path, _staging_generation|
        record_remote_change(staging_path, final_path, final_path, finalized_files) do
          Cloud.exec(Cloud::ObjectMove.command(staging_path, final_path))
        end
      end
    end

    def record_remote_change(source, target, generation_path, changes)
      # Defer signals until the remote change and generation are recorded for safe rollback.
      Thread.handle_interrupt(SignalException => :never) do
        yield
        changes << [source, target, nil]
        changes.last[2] = Cloud::ObjectMove.generation(generation_path)
      end
    end

    def rollback_uploads(staged_files, finalized_files)
      finalized_files.reverse_each.filter_map { |entry| attempt_finalized_rollback(*entry) } +
        cleanup_staged_uploads(staged_files, finalized_files)
    end

    def cleanup_staged_uploads(staged_files, finalized_files)
      finalized_staging_paths = finalized_files.map(&:first)
      # Finalization recreates staging with a new generation, so it is cleaned separately.
      staged_files.reject { |staged_file| finalized_staging_paths.include?(staged_file.first) }
                  .filter_map { |staging_path, _, generation| attempt_remote_cleanup(staging_path, generation) }
    end

    def attempt_finalized_rollback(staging_path, final_path, final_generation)
      rollback_error = attempt_remote_rollback(staging_path, final_path, final_generation)
      return rollback_error if rollback_error

      # Rollback copies final back to staging, so delete its newly read generation.
      attempt_remote_cleanup(staging_path, Cloud::ObjectMove.generation(staging_path))
    rescue StandardError => e
      e
    end

    # Try every remote cleanup so one failure does not block later restorations.
    def attempt_remote_rollback(source, target, target_generation)
      Cloud.exec(Cloud::ObjectMove.rollback_command(source, target, target_generation))
    rescue StandardError => e
      e
    end

    def attempt_remote_cleanup(staging_path, staging_generation)
      Cloud.exec(Cloud::ObjectMove.cleanup_command(staging_path, staging_generation))
    rescue StandardError => e
      e
    end
  end
end
