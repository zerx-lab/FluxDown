import unittest

from pr_review_title import BLOCKED, READY, title_with_status, text_verdict


class ReviewTitleTest(unittest.TestCase):
    def test_incremental_review_with_explicit_pass(self):
        body = (
            "## 审查结论（增量：`a13f3f39`）\n\n"
            "范围：仅审新提交。未发现阻塞合并的缺陷，以下均为非阻塞。\n\n"
            "### 低\n\n建议收紧匹配条件。"
        )
        self.assertEqual(text_verdict(body, is_review=True), READY)

    def test_review_without_clear_pass_replaces_stale_ready_prefix(self):
        body = (
            "## 审查结论\n\n整体设计可行。以下按严重度列出问题。\n\n"
            "### P2\n\n断连时可能误触发退出。\n\n"
            "### 已核对、无问题\n\n协议版本已同步。"
        )
        status = text_verdict(body, is_review=True)
        self.assertEqual(status, BLOCKED)
        self.assertEqual(
            title_with_status("【待合并】 feat: 支持自动退出", status),
            "【待处理】 feat: 支持自动退出",
        )
