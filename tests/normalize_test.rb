require "minitest/autorun"
require "securerandom"
require "shellwords"
require "stringio"
require_relative "../lib/normalize"

class NormalizeTest < Minitest::Test
  # Ensure project and bucket names remain single remote-shell arguments.
  def test_call_escapes_project_and_bucket_in_remote_commands
    project = "project;$(touch pwned)"
    bucket = "bucket;$(touch pwned)"
    commands, queries = run_normalization(project, bucket)

    assert_equal [Shellwords.join(["gcloud", "config", "set", "project", project])], commands
    assert_equal [Cloud::StorageApi.list_command(bucket)], queries
  end

  # Ensure normalized object names remain single remote-shell arguments during moves.
  def test_call_escapes_object_names_when_moving
    source = "gs://bucket/file;$(touch pwned)/é.txt"
    target = source.normalized
    commands = SecureRandom.stub(:hex, "token") { run_normalization_with_listing(source) }

    assert_equal expected_move_commands(source, target), commands
  end

  def test_call_passes_special_object_names_to_the_api
    source = "gs://bucket/folder*?[]#/é.txt"
    target = source.normalized
    commands = SecureRandom.stub(:hex, "token") { run_normalization_with_listing(source) }
    temporary = "#{source}.task-googlecloud-token"

    assert_api_move(commands, source, temporary)
    assert_api_move(commands, temporary, target)
  end

  # Ensure a collision found during preflight validation leaves Cloud Storage unchanged.
  def test_call_does_not_move_objects_when_normalized_names_collide
    commands = []
    queries = []

    stub_cloud_listing(commands, queries) do
      assert_raises(NormalizationPlan::CollisionError) do
        Cloud::Normalize.call("project", "bucket")
      end
    end

    assert_equal [Cloud::StorageApi.list_command("bucket")], queries
    refute(commands.any? { |command| command.include?("gsutil") })
  end

  private

  # Record Cloud commands while supplying a controlled Cloud Storage listing.
  def run_normalization(project, bucket)
    commands = []
    queries = []
    stub_cloud_listing(commands, queries, listing_pipe(queries, "gs://#{bucket}/file.txt")) do
      Cloud::Normalize.call(project, bucket)
    end
    [commands, queries]
  end

  def run_normalization_with_listing(source)
    commands = []
    queries = []
    Cloud::ObjectMove.stub(:move, recording_move(commands)) do
      stub_cloud_listing(commands, queries, listing_pipe(queries, source)) do
        Cloud::Normalize.call("project", "bucket")
      end
    end
    commands
  end

  def expected_move_commands(source, target)
    temporary = "#{source}.task-googlecloud-token"
    [
      Shellwords.join(%w[gcloud config set project project]),
      Cloud::ObjectMove.command(source, temporary, source_path: source, source_generation: "101"),
      Cloud::ObjectMove.command(temporary, target, source_path: temporary, source_generation: "101"),
    ]
  end

  def recording_move(commands)
    lambda do |source, target|
      commands << Cloud::ObjectMove.command(source, target, source_path: source, source_generation: "101")
      "101"
    end
  end

  def assert_api_move(commands, source, target)
    expected = Cloud::StorageApi.move_command(source, target, source_generation: "101")
    assert(commands.any? { |command| command.include?(expected) })
  end

  # Supply the exact object name needed to exercise a command path.
  def listing_pipe(queries, *objects)
    lambda do |command, &block|
      next block.call(StringIO.new("Generation: 101\n")) if command.start_with?("gsutil stat ")

      queries << command
      block.call(StringIO.new("#{objects.join("\n")}\n"))
    end
  end

  def stub_cloud_listing(commands, queries, pipe = colliding_object_pipe(queries), exec = nil, &)
    exec ||= ->(command) { commands << command }

    Cloud::Normalize.stub(:sleep, nil) do
      Cloud.stub(:login, nil) do
        Cloud.stub(:exec, exec) do
          Cloud.stub(:pipe, pipe, &)
        end
      end
    end
  end

  # Provide a gsutil ls response that includes both Unicode forms of the same name.
  def colliding_object_pipe(queries)
    objects = StringIO.new("gs://bucket/e\u0301.txt\ngs://bucket/é.txt\n")
    lambda do |command, &block|
      queries << command
      block.call(objects)
    end
  end
end
