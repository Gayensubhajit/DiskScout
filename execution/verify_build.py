#!/usr/bin/env python3
import subprocess
import sys

def run_step(name, cmd):
    print(f"==> Running {name} ({cmd})...")
    res = subprocess.run(cmd, shell=True, text=True)
    if res.returncode != 0:
        print(f"FAILED: {name} exited with code {res.returncode}")
        sys.exit(res.returncode)
    print(f"PASSED: {name}\n")

def main():
    run_step("cargo test", "cargo test")
    run_step("cargo check", "cargo check")
    run_step("cargo clippy", "cargo clippy -- -D warnings")
    print("ALL BUILD & TEST VERIFICATIONS PASSED!")

if __name__ == "__main__":
    main()
