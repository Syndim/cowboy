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
    failed, failed_prompts = scenario(root, "failed_session")

    full_blocks = [
        "role",
        "task",
        "recovery_context",
        "turn",
        "user_inputs",
        "deliverable",
    ]
    for records in [same, loaded]:
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

    assert failed["client"]["load_results"] == ["failure"] * 3
    session_id = failed["implement"]["session_id"]
    assert failed["client"]["load_attempts"] == [session_id] * 3
    assert failed["client"]["created_session_ids"] == [session_id]
    assert failed["client"]["prompt_sessions"] == [session_id]
    assert set(failed_prompts) == {"implement.prompt.txt"}
    assert failed["run"]["retries_used"] == 2
    assert failed["run"]["step"]["retries_used"]["revise"] == 2

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
