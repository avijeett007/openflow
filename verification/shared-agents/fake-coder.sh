#!/bin/bash
# A deliberately dumb stand-in coding agent. It is a REAL subprocess driven with
# the app's own PromptDelivery::Stdin contract: the instruction arrives on stdin,
# `--cwd {cwd}` is substituted by the app's own build_argv, and the edit it makes
# is what `git diff` in the grant folder proves afterwards.
set -u
cwd="."
while [ $# -gt 0 ]; do
  case "$1" in
    --cwd) cwd="$2"; shift 2 ;;
    *) shift ;;
  esac
done
instruction="$(cat)"
echo "coder: cwd=$cwd"
echo "coder: instruction=$instruction"
case "$instruction" in
  LONG*)
    echo "coder: this one takes a while"
    i=0
    while [ $i -lt 600 ]; do echo "coder: working $i"; sleep 1; i=$((i+1)); done
    ;;
  *)
    printf '<!-- %s -->\n' "$instruction" >> "$cwd/README.md"
    echo "coder: edited README.md"
    ;;
esac
echo "coder: done"
