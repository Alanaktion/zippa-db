#!/usr/bin/env python3
"""Adapt the fork's bundled queries to Rust regex and gpui-kit capture names.

Usage: python3 script/normalize-sql-highlights.py /path/to/zippa-db-tree-sitter
Run the fork's bundle-highlights.js first, as described in ui::sql_highlight.
"""
from pathlib import Path
import re
import sys

source = Path(sys.argv[1])
destination = Path(__file__).resolve().parents[1] / 'src/ui/sql_highlight/highlights'
for dialect in ('postgres', 'mysql', 'sqlite'):
    query = (source / dialect / 'queries/highlights.bundled.scm').read_text()
    for old, new in {
        'conditional': 'keyword', 'storageclass': 'keyword',
        'field': 'property', 'parameter': 'variable', 'float': 'number',
    }.items():
        query = re.sub(r'@' + old + r'\b', '@' + new, query)
    query = query.replace('%d', '[0-9]').replace(r'\.', '[.]')
    query = query.replace('(literal) @string',
                          '((literal) @string\n  (#not-match? @string "^(?i:true|false)$"))')
    header = '; Generated from zippa-db-tree-sitter 0.1.1; adapted by script/normalize-sql-highlights.py. See ui::sql_highlight.\n'
    (destination / f'{dialect}.scm').write_text(header + query)
