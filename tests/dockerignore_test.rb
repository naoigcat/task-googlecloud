require "minitest/autorun"

class DockerignoreTest < Minitest::Test
  def test_build_context_excludes_upload_data_and_repository_metadata
    ignored_paths = File.read(File.expand_path("../.dockerignore", __dir__)).lines(chomp: true)

    assert_includes ignored_paths, "uploads/"
    assert_includes ignored_paths, ".git/"
    assert_includes ignored_paths, ".DS_Store"
  end
end
