/* Throughput / dictionary-cost probe.  tp LEVEL DICT|- LIST REPS   (LIST: one file path per line; files > 4 MiB are cut into 4 MiB chunks)
 * Build: gcc -O2 tp.c -o tp -lzstd.  Output: one CSV line. Pin with taskset. */
#include <zstd.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
static double now(void){struct timespec t;clock_gettime(CLOCK_MONOTONIC,&t);return t.tv_sec+t.tv_nsec*1e-9;}
static int cmpd(const void*a,const void*b){double x=*(double*)a,y=*(double*)b;return x<y?-1:x>y;}
int main(int argc,char**argv){
  if(argc<5)return 2; int level=atoi(argv[1]); const char*dp=argv[2]; int reps=atoi(argv[4]);
  enum{CH=4<<20}; size_t cap=1024,n=0; char**src=malloc(cap*sizeof*src); size_t*sl=malloc(cap*sizeof*sl);
  FILE*lf=fopen(argv[3],"r"); char line[4096];
  while(fgets(line,sizeof line,lf)){line[strcspn(line,"\n")]=0; FILE*f=fopen(line,"rb"); if(!f)continue;
    for(;;){char*b=malloc(CH); size_t r=fread(b,1,CH,f); if(!r){free(b);break;} if(n==cap){cap*=2;src=realloc(src,cap*sizeof*src);sl=realloc(sl,cap*sizeof*sl);} src[n]=b;sl[n++]=r; if(r<CH)break;} fclose(f);}
  void*dict=NULL; size_t dl=0; if(strcmp(dp,"-")){FILE*d=fopen(dp,"rb"); fseek(d,0,SEEK_END); dl=ftell(d); rewind(d); dict=malloc(dl); fread(dict,1,dl,d); fclose(d);}
  double t0=now(); ZSTD_CDict*cd=dict?ZSTD_createCDict(dict,dl,level):NULL; double t_cd=now()-t0;
  t0=now(); ZSTD_DDict*dd=dict?ZSTD_createDDict(dict,dl):NULL; double t_dd=now()-t0;
  size_t m_cd=cd?ZSTD_sizeof_CDict(cd):0, m_dd=dd?ZSTD_sizeof_DDict(dd):0;
  ZSTD_CCtx*cc=ZSTD_createCCtx(); ZSTD_DCtx*dc=ZSTD_createDCtx();
  size_t bound=ZSTD_compressBound(CH); char**z=malloc(n*sizeof*z); size_t*zl=malloc(n*sizeof*zl); char*out=malloc(CH);
  for(size_t i=0;i<n;i++)z[i]=malloc(ZSTD_compressBound(sl[i]));
  size_t tin=0,tout=0; for(size_t i=0;i<n;i++)tin+=sl[i];
  double*ct=malloc(reps*sizeof(double)),*dt=malloc(reps*sizeof(double));
  for(int r=-1;r<reps;r++){ /* r=-1 is the warm-up pass */
    t0=now(); tout=0;
    for(size_t i=0;i<n;i++){ zl[i]=cd?ZSTD_compress_usingCDict(cc,z[i],ZSTD_compressBound(sl[i]),src[i],sl[i],cd):ZSTD_compressCCtx(cc,z[i],ZSTD_compressBound(sl[i]),src[i],sl[i],level); tout+=zl[i]; }
    double a=now()-t0; t0=now();
    for(size_t i=0;i<n;i++){ size_t k=dd?ZSTD_decompress_usingDDict(dc,out,CH,z[i],zl[i],dd):ZSTD_decompressDCtx(dc,out,CH,z[i],zl[i]); if(k!=sl[i]){fprintf(stderr,"roundtrip fail\n");return 1;} }
    double b=now()-t0; if(r>=0){ct[r]=a;dt[r]=b;} }
  qsort(ct,reps,sizeof(double),cmpd); qsort(dt,reps,sizeof(double),cmpd);
  double mb=tin/1e6;
  printf("%d,%s,%zu,%zu,%zu,%.1f,%.1f,%.1f,%.1f,%.1f,%.1f,%.1f,%.1f,%zu,%zu\n",level,dict?"dict":"nodict",n,tin,tout,
    mb/ct[reps/2],mb/ct[reps-1],mb/ct[0],mb/dt[reps/2],mb/dt[reps-1],mb/dt[0],t_cd*1e3,t_dd*1e3,m_cd,m_dd);
  return 0; }
