#!/usr/bin/env python3
import os
import sys
import shutil
from pathlib import Path
import json

def inspect_trash():
    home = Path.home()
    trash_dir = home / ".local/share/Trash"
    files_dir = trash_dir / "files"
    
    total_size = 0
    file_count = 0
    if files_dir.exists():
        for p in files_dir.rglob("*"):
            if p.is_file() and not p.is_symlink():
                try:
                    total_size += p.stat().st_size
                    file_count += 1
                except (PermissionError, FileNotFoundError):
                    pass
    return {"path": str(trash_dir), "file_count": file_count, "total_bytes": total_size}

def inspect_cache():
    home = Path.home()
    cache_dir = home / ".cache"
    total_size = 0
    entries = []
    if cache_dir.exists():
        for item in cache_dir.iterdir():
            size = 0
            if item.is_dir():
                for p in item.rglob("*"):
                    try:
                        if p.is_file() and not p.is_symlink():
                            size += p.stat().st_size
                    except (PermissionError, FileNotFoundError):
                        pass
            elif item.is_file():
                try:
                    size = item.stat().st_size
                except (PermissionError, FileNotFoundError):
                    pass
            total_size += size
            entries.append({"name": item.name, "bytes": size})
    entries.sort(key=lambda x: x["bytes"], reverse=True)
    return {"total_bytes": total_size, "top_entries": entries[:10]}

if __name__ == "__main__":
    summary = {
        "trash": inspect_trash(),
        "cache": inspect_cache()
    }
    print(json.dumps(summary, indent=2))
