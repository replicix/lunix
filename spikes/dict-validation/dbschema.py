"""Schemas of the five datasets (section 2.3) and DDL generation for PostgreSQL, MySQL and SQLite.
kinds: i int, b bigint, d double, t timestamp/datetime, s short string, x long text, c char(n)"""
SE = {
 'Posts': (['Id i','PostTypeId i','AcceptedAnswerId i','ParentId i','CreationDate t','Score i','ViewCount i','Body x','OwnerUserId i','OwnerDisplayName s',
            'LastEditorUserId i','LastEditorDisplayName s','LastEditDate t','LastActivityDate t','Title s','Tags s','AnswerCount i','CommentCount i','FavoriteCount i',
            'ClosedDate t','CommunityOwnedDate t','ContentLicense s'], 'Id'),
 'Comments': (['Id i','PostId i','Score i','Text x','CreationDate t','UserDisplayName s','UserId i','ContentLicense s'], 'Id'),
 'Users': (['Id i','Reputation i','CreationDate t','DisplayName s','LastAccessDate t','WebsiteUrl s','Location s','AboutMe x','Views i','UpVotes i','DownVotes i','ProfileImageUrl s','AccountId i'], 'Id'),
 'Votes': (['Id i','PostId i','VoteTypeId i','UserId i','CreationDate t','BountyAmount i'], 'Id'),
 'Badges': (['Id i','UserId i','Name s','Date t','Class i','TagBased s'], 'Id'),
 'PostLinks': (['Id i','CreationDate t','PostId i','RelatedPostId i','LinkTypeId i'], 'Id'),
 'Tags': (['Id i','TagName s','Count i','ExcerptPostId i','WikiPostId i'], 'Id'),
}
SE_IDX = ['Posts(OwnerUserId)', 'Posts(ParentId)', 'Posts(CreationDate)', 'Comments(PostId)', 'Votes(PostId)', 'Badges(UserId)']
GEO = {'geoname': (['geonameid i','name s','asciiname s','alternatenames x','latitude d','longitude d','feature_class c','feature_code s','country_code c','cc2 s',
                    'admin1_code s','admin2_code s','admin3_code s','admin4_code s','population b','elevation i','dem i','timezone s','modification_date s'], 'geonameid')}
GEO_IDX = ['geoname(country_code)', 'geoname(feature_class, feature_code)', 'geoname(latitude, longitude)', 'geoname(name)']
TLC_IDX = ['trips(tpep_pickup_datetime)', 'trips(PULocationID)', 'trips(DOLocationID)']
def tlc_schema(cols):
    return {'trips': (cols, None)}
DATASETS = {'se-askubuntu': (SE, SE_IDX), 'se-superuser': (SE, SE_IDX), 'se-serverfault': (SE, SE_IDX), 'geonames': (GEO, GEO_IDX)}
def ddl(engine, schema, idx):
    out = []
    for t, (cols, pk) in schema.items():
        defs = []
        for c in cols:
            n, k = c.split()
            ty = {'pg': {'i':'integer','b':'bigint','d':'double precision','t':'timestamp','s':'text','x':'text','c':'text'},
                  'my': {'i':'int','b':'bigint','d':'double','t':'datetime(3)','s':'varchar(512)','x':'mediumtext','c':'char(8)'},
                  'sq': {'i':'integer','b':'integer','d':'real','t':'text','s':'text','x':'text','c':'text'}}[engine][k]
            if k == 'i' and engine in ('pg', 'my') and n in ('Reputation',): ty = 'bigint' if engine == 'pg' else 'bigint'
            defs.append('"%s" %s' % (n, ty) if engine != 'my' else '`%s` %s' % (n, ty))
        if pk: defs.append('PRIMARY KEY ("%s")' % pk if engine != 'my' else 'PRIMARY KEY (`%s`)' % pk)
        out.append('CREATE TABLE %s (%s)%s;' % (t if engine != 'my' else '`%s`' % t, ', '.join(defs), ' ENGINE=InnoDB' if engine == 'my' else ''))
    ix = []
    for i, s in enumerate(idx):
        t, c = s[:-1].split('(')
        q = (lambda x: '"%s"' % x) if engine != 'my' else (lambda x: '`%s`' % x)
        ix.append('CREATE INDEX i%d ON %s (%s);' % (i, q(t) if engine == 'my' else t, ', '.join(q(x.strip()) for x in c.split(','))))
    return out, ix
