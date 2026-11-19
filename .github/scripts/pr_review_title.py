#!/usr/bin/env python3
"""Sync open PR title prefixes from Roost reviews, merge conflicts and CI failures."""

import argparse
import json
import os
import re
import subprocess

BOT = "roost-bot[bot]"
BLOCKED = "待处理"
READY = "待合并"
PREFIX = re.compile(r"^(?:【(?:待处理|待合并)】\s*)+")
GATE = re.compile(r"^<!--\s*roost:gate:(pass|fail):[0-9a-f]{40}\s*-->")
CONCLUSION = re.compile(r"^(?:结论|conclusion|verdict|review summary)\s*[：:]?\s*", re.I)
BLOCKER_HEADING = re.compile(
    r"^(?:\d+[.、]\s*)?(?:阻塞(?:项|问题|缺陷)?|blocking (?:issues?|findings)|blockers?)"
    r"(?:[：:]|\s|$)", re.I
)
BLOCKING = re.compile(
    r"(?:仍)?(?:需(?:要)?|必须)(?:修改|修复|改[。；，、\s]|改$)"
    r"|(?:不能|不建议|不可|不宜)合|修完再合|等.{0,40}处理再合"
    r"|阻塞(?:项|问题|缺陷).{0,65}(?:仍未|未处理|未解决|未消除|未闭合)"
    r"|(?:有|剩|发现)\s*(?:[一二三四五两1-9]\s*(?:[个处条]\s*)?)?(?:新的?)?阻塞"
    r"|\b(?:changes required|do not merge|must fix|blocking (?:issue|finding)s?)\b", re.I
)
NONBLOCKING = re.compile(
    r"(?:无|没有(?:发现)?|未发现)(?:功能性)?(?:新增|新的?)?\s*阻塞(?:项|问题|缺陷)?"
    r"|(?:上一轮|上轮)的阻塞(?:项|问题|缺陷)?.{0,40}?(?:已解决|已消除|已修好)"
    r"|非阻塞(?:项|问题|缺陷)?|不阻塞|无(?:新增|新的?)?(?:缺陷|问题)|没有(?:新)?问题"
    r"|(?<![不无未])可(?:以)?合(?:并)?|\b(?:no (?:new )?(?:blocking (?:issue|finding)s?|blockers?|issues?)"
    r"|non[- ]blocking|ready to merge|lgtm)\b", re.I
)
NO_DIFF = re.compile(
    r"没有新(?:的)?提交|无新提交|没有新的功能改动|增量\s*diff\s*为空|范围为空|无新增可审内容"
)
EMPTY_FINDINGS = re.compile(r"^(?:无|没有|none|no findings|0)[。.!！]?$", re.I)

PR_DETAILS = """
query($owner: String!, $name: String!, $number: Int!) {
  repository(owner: $owner, name: $name) {
    pullRequest(number: $number) {
      state
      title
      mergeable
      commits(last: 1) {
        nodes {
          commit {
            statusCheckRollup {
              contexts {
                checkRunCountsByState { state count }
                statusContextCountsByState { state count }
              }
            }
          }
        }
      }
    }
  }
}
"""
CI_ERRORS = {"FAILURE", "ERROR", "TIMED_OUT", "STARTUP_FAILURE", "ACTION_REQUIRED"}


def review_status(entry):
    """Return (timestamp, id, verdict) for an actual review, or None for chatter."""
    if (entry.get("user") or {}).get("login") != BOT:
        return None
    state = entry.get("state")
    if state in {"PENDING", "DISMISSED"}:
        return None
    timestamp = entry.get("submitted_at") or entry.get("created_at")
    if not timestamp:
        return None
    body = (entry.get("body") or "").strip()
    gate = GATE.match(body)
    if gate:
        verdict = READY if gate[1] == "pass" else BLOCKED
    elif body.startswith("<!-- roost:"):
        return None  # Auto-revision, CI, and handoff notices are not reviews.
    elif state == "CHANGES_REQUESTED":
        verdict = BLOCKED
    elif state == "APPROVED":
        verdict = READY
    else:
        verdict = text_verdict(body, is_review=state == "COMMENTED")
        if verdict is None:
            return None
    return timestamp, entry["id"], verdict


def text_verdict(body, *, is_review):
    lines = [line.strip().replace("**", "") for line in body.splitlines() if line.strip()]
    if not lines:
        return None
    first = lines[0].lstrip("# ")
    conclusion = CONCLUSION.match(first)
    if conclusion:
        summary = [first[conclusion.end():]]
        for line in lines[1:]:
            if line.startswith("#"):
                break
            summary.append(line)
        summary = " ".join(summary)
    elif is_review:
        summary = first
    else:
        return None  # Ordinary PR comments need an explicit review conclusion.

    empty_blockers = False
    for i, line in enumerate(lines):
        heading = BLOCKER_HEADING.match(line.lstrip("# ")) if line.startswith("#") else None
        if heading:
            detail = line.lstrip("# ")[heading.end():].strip(" ：:")
            if not detail and i + 1 < len(lines) and not lines[i + 1].startswith("#"):
                detail = lines[i + 1]
            if not EMPTY_FINDINGS.fullmatch(detail):
                return BLOCKED
            empty_blockers = True
    if BLOCKING.search(NONBLOCKING.sub("", summary)):
        return BLOCKED
    if NO_DIFF.search(summary):
        return None  # A no-op synchronize review does not clear a previous verdict.
    if empty_blockers or NONBLOCKING.search(summary):
        return READY
    return ""  # Latest substantive review is ambiguous: leave the title as-is.


def title_with_status(title, status):
    if status:
        return f"【{status}】 {PREFIX.sub('', title)}"
    return title


def gh(*args):
    return subprocess.run(["gh", *args], check=True, stdout=subprocess.PIPE, text=True).stdout


def pages(endpoint):
    separator = "&" if "?" in endpoint else "?"
    return json.loads(gh("api", "--paginate", "--slurp", f"{endpoint}{separator}per_page=100"))


def pr_details(repo, number):
    owner, name = repo.split("/")
    return json.loads(gh(
        "api", "graphql", "-f", f"query={PR_DETAILS}",
        "-f", f"owner={owner}", "-f", f"name={name}", "-F", f"number={number}",
    ))["data"]["repository"]["pullRequest"]


def status_with_checks(review, pr):
    if pr["mergeable"] == "CONFLICTING":
        return BLOCKED
    # Read the current head's complete rollup, not CI history from older commits.
    for node in pr["commits"]["nodes"]:
        rollup = node["commit"]["statusCheckRollup"]
        if rollup:
            for field in ("checkRunCountsByState", "statusContextCountsByState"):
                if any(row["count"] and row["state"] in CI_ERRORS for row in rollup["contexts"][field]):
                    return BLOCKED
    if review == READY and pr["mergeable"] == "UNKNOWN":
        return None  # Don't promote while GitHub is still computing mergeability.
    return review


def sync(repo, *, dry_run):
    for page in pages(f"repos/{repo}/pulls?state=open"):
        for pr in page:
            number = pr["number"]
            endpoint = f"repos/{repo}/pulls/{number}"
            verdicts = (
                verdict
                for source in (f"{endpoint}/reviews", f"repos/{repo}/issues/{number}/comments")
                for batch in pages(source)
                for entry in batch
                if (verdict := review_status(entry)) is not None
            )
            _, _, status = max(verdicts, default=("", 0, None))
            if status is None:
                continue  # Only PRs with a Roost review are eligible for updates.
            current = pr_details(repo, number)
            if current["state"] != "OPEN":
                continue
            status = status_with_checks(status, current)
            title = title_with_status(current["title"], status)
            if title == current["title"]:
                continue
            if not dry_run:
                gh("api", "--method", "PATCH", endpoint, "--raw-field", f"title={title}")
            action = "would set" if dry_run else "set"
            print(f"#{number}: {action} 【{status}】")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--dry-run", action="store_true", help="Report planned updates without writing")
    sync(os.environ["GH_REPO"], dry_run=parser.parse_args().dry_run)
