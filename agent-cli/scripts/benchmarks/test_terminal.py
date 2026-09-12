import unittest

from startup import Display, QUERIES


class TerminalTests(unittest.TestCase):
    def test_ca_ad_8_split_terminal_queries_are_replied_to_once(self):
        for query, reply in QUERIES.items():
            for boundary in range(len(query) + 1):
                with self.subTest(query=query, boundary=boundary):
                    replies = []
                    display = Display(replies.append)
                    display.feed("before" + query[:boundary])
                    display.feed(query[boundary:] + "after")
                    self.assertEqual(replies, [reply])
                    self.assertTrue(display.text().startswith("beforeafter"))

    def test_ca_ad_8_synchronized_frames_and_exact_cursor_are_observable(self):
        display = Display(lambda _: None)
        display.feed("\x1b[?2026h\x1b[12;4Hprobe")
        self.assertIn(2026 << 5, display.screen.mode)
        display.feed("\x1b[?2026l\x1b[12;4H\x1b[K")
        self.assertNotIn(2026 << 5, display.screen.mode)
        self.assertEqual((display.screen.cursor.y, display.screen.cursor.x), (11, 3))
        self.assertNotIn("probe", display.text())


if __name__ == "__main__":
    unittest.main()
