#!/bin/sh
# Show what an agent is doing on the lamp, through lampbus-life. Safe to call from hooks: it never fails the caller.
#   agent-presence.sh working | done | needs-you | error | idle
command -v lampbus-life >/dev/null 2>&1 || exit 0
case "${1:-}" in
  working)   lampbus-life mood focus >/dev/null 2>&1 ;;
  done)      lampbus-life poke good  >/dev/null 2>&1; lampbus-life mood ember >/dev/null 2>&1 ;;
  needs-you) lampbus-life poke warn  >/dev/null 2>&1 ;;
  error)     lampbus-life poke bad   >/dev/null 2>&1 ;;
  idle)      lampbus-life mood ember >/dev/null 2>&1 ;;
  *) echo "usage: agent-presence.sh working|done|needs-you|error|idle" >&2; exit 2 ;;
esac
exit 0
