from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
RELEASE_DIST_WORKFLOW = ROOT / ".github" / "workflows" / "release-dist.yml"


def test_skills_archive_validation_does_not_sigpipe_tar_under_pipefail():
    workflow = RELEASE_DIST_WORKFLOW.read_text()
    smoke_step = workflow.split(
        "- name: Smoke-test linux x86_64 binary + skills bundle", 1
    )[1].split("- name: Upload dist artifacts", 1)[0]

    assert 'skill_entries="$(tar -tzf "${skills[0]}")"' in smoke_step
    assert '<<<"$skill_entries"' in smoke_step
    assert "tar -tzf" not in smoke_step.split(
        'skill_entries="$(tar -tzf "${skills[0]}")"', 1
    )[1]
    assert "| grep -q" not in smoke_step
    assert "| head" not in smoke_step


def test_release_builds_architecture_specific_macos_artifacts():
    workflow = RELEASE_DIST_WORKFLOW.read_text()
    platform_list = "linux,linux-x64,macos-arm64,macos-x64,windows"

    assert f'default: "{platform_list}"' in workflow
    assert workflow.count(f"inputs.platforms || '{platform_list}'") == 2
    assert "universal2" not in workflow
