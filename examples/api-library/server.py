#!/usr/bin/env python3
"""Reuse the checked-in synthetic inventory API; no live services or user values."""
from pathlib import Path
import runpy
import sys

API_IMPORT = Path(__file__).resolve().parents[1] / 'api-import'
sys.path.insert(0, str(API_IMPORT))
if __name__ == '__main__':
    runpy.run_path(str(API_IMPORT / 'server.py'), run_name='__main__')
