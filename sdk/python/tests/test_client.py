import unittest
from obscura_next.models import SessionConfig, AgentAction, ActionResult, SemanticNode

class TestObscuraNextSDK(unittest.TestCase):
    def test_models_instantiation(self):
        cfg = SessionConfig(stealth=True, timeout_ms=10000)
        self.assertTrue(cfg.stealth)
        self.assertEqual(cfg.timeout_ms, 10000)

        act = AgentAction(action="click", selector="#submit")
        self.assertEqual(act.action, "click")
        self.assertEqual(act.selector, "#submit")

        res = ActionResult(action="click", success=True, payload={"clicked": True}, elapsed_ms=12.4)
        self.assertTrue(res.success)
        self.assertEqual(res.elapsed_ms, 12.4)

        node = SemanticNode(id="@e1", tag="button", text="Submit", selector="#submit", is_interactive=True)
        self.assertTrue(node.is_interactive)
        self.assertEqual(node.tag, "button")

if __name__ == "__main__":
    unittest.main()
