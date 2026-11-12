"""Extension/name -> type map (the 'extension map' under test), families, shared constants."""
import os

HEADER_LEN = 16           # chunkfmt HEADER_LEN
CHUNK = 4 << 20

# type -> extensions (lowercase, no dot).  Order matters only for documentation.
EXT = {
 'c': ['c'], 'h': ['h'], 'cpp': ['cc','cpp','cxx','c++','hpp','hh','hxx','ipp','tpp','inl'],
 'rust': ['rs'], 'go': ['go'], 'python': ['py','pyi'], 'java': ['java'],
 'js': ['js','mjs','cjs','jsx'], 'ts': ['ts','tsx','mts','cts'],
 'ruby': ['rb','rake','gemspec'], 'php': ['php'], 'perl': ['pl','pm','t'],
 'sh': ['sh','bash','zsh'], 'csharp': ['cs'], 'kotlin': ['kt','kts'], 'swift': ['swift'],
 'scala': ['scala','sc'], 'haskell': ['hs'], 'lua': ['lua'], 'r': ['r'], 'sql': ['sql'],
 'fortran': ['f','f90','f95','f03','for'], 'ada': ['adb','ads'], 'cobol': ['cbl','cob','cpy'],
 'erlang': ['erl','hrl'], 'elixir': ['ex','exs'], 'asm': ['s','asm'],
 'make': ['mk'], 'cmake': ['cmake'],
 'json': ['json'], 'jsonl': ['jsonl','ndjson'], 'yaml': ['yml','yaml'], 'toml': ['toml'],
 'xml': ['xml','xsd','xsl','xslt','plist','csproj','props','targets','resx'],
 'html': ['html','htm','xhtml'], 'css': ['css','scss','less'], 'md': ['md','markdown'],
 'rst': ['rst'], 'tex': ['tex','sty','cls','bib','dtx'], 'csv': ['csv','tsv'],
 'ini': ['ini','cfg','conf','properties','service','desktop'], 'svg': ['svg'],
 'pbtxt': ['pbtxt','textproto','prototxt','pb_text'], 'po': ['po','pot'],
 # binaries and controls
 'elf': ['o','so','a'], 'class': ['class'], 'wasm': ['wasm'], 'pdf': ['pdf'],
 'sqlite': ['sqlite','sqlite3','db'],
 'png': ['png'], 'jpeg': ['jpg','jpeg'], 'zip': ['zip','jar','docx','xlsx','pptx','whl','apk'],
 'gz': ['gz','tgz','bz2','xz','zst'], 'video': ['mp4','webm','mkv','avi','mov','ogv','mp3','ogg','gif','woff','woff2'],
}
NAME = {  # whole-file-name rules
 'Makefile':'make','makefile':'make','GNUmakefile':'make','CMakeLists.txt':'cmake',
 'Rakefile':'ruby','Gemfile':'ruby','Dockerfile':'dockerfile',
}
EXT_TO_TYPE = {}
for _t, _es in EXT.items():
    for _e in _es: EXT_TO_TYPE[_e] = _t

def ext_of(name):
    b = os.path.basename(name)
    if '.' not in b.lstrip('.'): return ''
    return b.rsplit('.',1)[1]            # case preserved (.R vs .r, .S vs .s)

def classify_name(name):
    """The extension map. Returns a type or None.  Case-sensitive exceptions first."""
    b = os.path.basename(name)
    if b in NAME: return NAME[b]
    if b.startswith('Dockerfile') or b.endswith('.dockerfile'): return 'dockerfile'
    if b.lower().endswith(('.tar.gz','.tar.xz','.tar.bz2')): return 'gz'
    e = ext_of(b)
    if e == 'S': return 'asm'
    if e == 'R': return 'r'
    return EXT_TO_TYPE.get(e.lower())

TEXT_TYPES = ['c','h','cpp','rust','go','python','java','js','ts','ruby','php','perl','sh','csharp','kotlin','swift',
 'scala','haskell','lua','r','sql','fortran','ada','cobol','erlang','elixir','asm','make','cmake','dockerfile',
 'json','jsonl','yaml','toml','xml','html','css','md','rst','tex','csv','ini','svg','pbtxt','po',
 'syslog','access','applog']
BIN_TYPES = ['elf','class','wasm','pdf','sqlite']
CONTROL_TYPES = ['png','jpeg','zip','gz','video']
ALL_TYPES = TEXT_TYPES + BIN_TYPES + CONTROL_TYPES

# a-priori families (decided before looking at results; a data-driven clustering is reported next to it)
FAMILY = {}
for t in ['c','h','cpp','java','js','ts','csharp','kotlin','swift','scala','go','rust','php']: FAMILY[t]='clike'
for t in ['python','ruby','perl','sh','lua','r','elixir']: FAMILY[t]='script'
for t in ['haskell','erlang']: FAMILY[t]='func'
for t in ['sql','fortran','ada','cobol','asm','make','cmake','dockerfile']: FAMILY[t]='other-code'
for t in ['html','xml','svg','css','md','rst','tex','po']: FAMILY[t]='markup'
for t in ['json','jsonl','yaml','toml','csv','ini','pbtxt']: FAMILY[t]='data'
for t in ['syslog','access','applog']: FAMILY[t]='log'
for t in BIN_TYPES: FAMILY[t]='binary'
for t in CONTROL_TYPES: FAMILY[t]='control'

# ext variants with separate dictionaries (per-extension experiment)
EXT_VARIANTS = {'h':['h'], 'ts':['ts','tsx'], 'json':['json'], 'jsonl':['jsonl'], 'js':['js','jsx'], 'cpp':['cc','cpp','hpp'], 'css':['css','scss','less'],
                'xml':['xml','csproj'], 'ini':['ini','cfg','conf','properties'], 'fortran':['f','f90']}
# Licence policy (engineering assumption from the maintainer, to be confirmed by counsel; not a legal conclusion).
# 'perm'    : permissive or public domain
# 'copyleft': allowed in shippable training if the shipped data is GPL-3.0-or-later (MPL-2.0, LGPL, GPL-2.0-or-later, GPL-3.0, AGPL-3.0)
# 'excluded': test only (GPL-2.0-only, CC-BY/-SA/GFDL, non-commercial, SSPL/BSL, proprietary, unclear)
PERM = {'MIT','BSD-2-Clause','BSD-3-Clause','Apache-2.0','ISC','Zlib','CC0-1.0','Unlicense','Public-Domain','0BSD',
        'PostgreSQL','Apache-2.0-LLVM','curl','libpng','Python-PSF','BSL-1.0','MIT-or-Apache-2.0'}
COPYLEFT_OK = {'MPL-2.0','LGPL-2.1-or-later','LGPL-2.1-only','LGPL-3.0','LGPL-2.0-or-later','GPL-2.0-or-later','GPL-3.0-only','GPL-3.0-or-later','AGPL-3.0'}
def policy(spdx):
    if spdx in PERM: return 'perm'
    if spdx in COPYLEFT_OK: return 'copyleft'
    return 'excluded'
