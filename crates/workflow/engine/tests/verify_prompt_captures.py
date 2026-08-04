import json
import sys
from pathlib import Path


def scenario(root: Path, name: str):
    base = root / name
    records = json.loads((base / "records.json").read_text(encoding="utf-8"))
    prompts = {
        path.name: path.read_text(encoding="utf-8")
        for path in base.glob("*.prompt.txt")
    }
    assert prompts and all(prompts.values()), f"missing/empty prompts for {name}"
    return records, prompts


def main() -> None:
    root = Path(sys.argv[1])
    same, same_prompts = scenario(root, "same_runtime")
    retry, retry_prompts = scenario(root, "retry")
    loaded, loaded_prompts = scenario(root, "loaded_session")
    recreated, recreated_prompts = scenario(root, "recreated_session")

    full_blocks = [
        "role",
        "task",
        "recovery_context",
        "turn",
        "user_inputs",
        "deliverable",
    ]
    for records in [same, loaded, recreated]:
        assert records["implement"]["task_key"] == "implementation"
        assert records["revise"]["task_key"] == "implementation"
        assert (
            records["implement"]["fingerprint"] == records["revise"]["fingerprint"]
        )
        assert records["implement"]["prompt_blocks"] == full_blocks

    for records, prompts in [
        (same, same_prompts),
        (loaded, loaded_prompts),
    ]:
        revision = prompts["revise.prompt.txt"]
        assert records["revise"]["prompt_blocks"] == ["turn"]
        assert "Changes needed:" in revision
        assert "Context:" in revision
        for heading in [
            "## Role",
            "## Task",
            "## Recovery Context",
            "## Deliverable Format",
        ]:
            assert heading not in revision, (heading, revision)
        assert len(revision) * 2 < len(prompts["implement.prompt.txt"])

    assert same["implement"]["session_id"] == same["revise"]["session_id"]
    assert loaded["implement"]["session_id"] == loaded["revise"]["session_id"]
    assert loaded["client"]["load_results"] == ["success"]

    recreated_revision = recreated_prompts["revise.prompt.txt"]
    assert recreated["client"]["load_results"] == ["failure"]
    assert recreated["implement"]["session_id"] != recreated["revise"]["session_id"]
    assert recreated["revise"]["prompt_blocks"] == full_blocks
    for heading in [
        "## Role",
        "## Task",
        "## Recovery Context",
        "## Current Turn",
        "## Deliverable Format",
    ]:
        assert heading in recreated_revision
    assert "Changes needed:" in recreated_revision
    assert "Context:" in recreated_revision

    assert retry["initial"]["session_id"] == retry["retry"]["session_id"]
    assert retry["initial"]["fingerprint"] == retry["retry"]["fingerprint"]
    assert retry["retry"]["prompt_blocks"] == ["retry"]
    retry_prompt = retry_prompts["retry.prompt.txt"]
    assert "## Retry" in retry_prompt
    for heading in [
        "## Role",
        "## Task",
        "## Recovery Context",
        "## Current Turn",
        "## Deliverable Format",
    ]:
        assert heading not in retry_prompt

    print("persisted workflow prompt comparisons passed")


if __name__ == "__main__":
    main()
