#include <mmx/Block.hxx>
#include <mmx/ProofOfSpaceOG.hxx>
#include <mmx/ProofOfSpaceNFT.hxx>
#include <mmx/hash_512_t.hpp>
#include <mmx/pos/verify.h>
#include <mmx/pos/mem_hash.h>
#include <mmx/utils.h>
#include <vnx/vnx.h>
#include <fstream>
#include <iomanip>
#include <sstream>
namespace mmx {
struct WorkReference {
    std::shared_ptr<ChainParams> params;
    std::shared_ptr<BlockHeader> parent;
    std::vector<uint64_t> iterations;
    hash_t challenge;
    uint64_t difficulty=1;
    std::shared_ptr<const BlockHeader> find_prev(std::shared_ptr<const BlockHeader> b) const {return b->prev==parent->hash?parent:nullptr;}
    hash_t get_infusion(std::shared_ptr<const BlockHeader>,uint32_t offset,uint64_t& out)const {out=iterations.at(offset);return parent->hash;}
    hash_t get_challenge(std::shared_ptr<const BlockHeader>,uint32_t,uint64_t& out)const {out=difficulty;return challenge;}
    void validate_diff_adjust(const uint64_t&,const uint64_t&)const;
    void verify_proof(std::shared_ptr<const BlockHeader>)const;
    void verify_proof(std::shared_ptr<const ProofOfSpace>,const hash_t&,uint64_t,const uint32_t&)const;
    template<class T>void verify_proof_impl(std::shared_ptr<const T>,const hash_t&,uint64_t,const uint32_t&)const;
};
// MMX_WORK_METHODS
struct HistoryReference {
    std::shared_ptr<ChainParams> params;
    std::map<hash_t,std::shared_ptr<BlockHeader>> headers;
    std::shared_ptr<const BlockHeader> get_header(const hash_t& h)const {auto it=headers.find(h);return it==headers.end()?nullptr:it->second;}
    std::shared_ptr<const BlockHeader> find_prev(std::shared_ptr<const BlockHeader>,size_t distance=1,bool clamped=false)const;
    bool find_challenge(std::shared_ptr<const BlockHeader>,uint32_t,hash_t&,uint64_t&)const;
    hash_t get_challenge(std::shared_ptr<const BlockHeader>,uint32_t,uint64_t&)const;
    uint64_t get_time_diff(std::shared_ptr<const BlockHeader>)const;
    bool find_infusion(std::shared_ptr<const BlockHeader>,uint32_t,hash_t&,uint64_t&)const;
    hash_t get_infusion(std::shared_ptr<const BlockHeader>,uint32_t,uint64_t&)const;
};
// MMX_HISTORY_METHODS
namespace pos {
void compute_f1(std::vector<uint32_t>*,std::vector<uint32_t>&,std::vector<std::array<uint32_t,N_META>>&,std::mutex&,uint32_t,const hash_t&,int,int);
// MMX_CANDIDATE
}
}
using namespace mmx;
std::string hex(const uint8_t* bytes,size_t n){std::ostringstream s;s<<std::hex<<std::setfill('0');for(size_t i=0;i<n;++i)s<<std::setw(2)<<unsigned(bytes[i]);return s.str();}
template<size_t N>std::string hx(const bytes_t<N>& h){return hex(h.data(),N);}
hash_t addr(uint64_t v){hash_t h;h.from_uint(v);return h;}
template<class F>std::string outcome(F f){try{f();return "OK";}catch(const std::exception&e){return e.what();}}
template<class F>std::string hash_outcome(F f){try{return hx(f());}catch(const std::exception&e){return std::string("ERR:")+e.what();}}
std::string wire(std::shared_ptr<const vnx::Value> v){std::vector<uint8_t>b;vnx::VectorOutputStream s(&b);vnx::TypeOutput out(&s);vnx::write(out,v);out.flush();return hex(b.data(),b.size());}
std::ofstream output;size_t count=0;
void emit(vnx::Object row){output<<vnx::to_string(row)<<'\n';++count;}
vnx::Object computed(const std::vector<uint32_t>& xs,const hash_t& id,unsigned ksize){
    vnx::Object row;row["kind"]="compute";row["xs"]=xs;row["id"]=hx(id);row["ksize"]=ksize;
    std::vector<uint32_t> ordered;std::vector<vnx::Object> entries;
    row["result"]=outcome([&]{const auto res=pos::compute(xs,&ordered,id,ksize,0);for(size_t i=0;i<res.size();++i){vnx::Object e;e["y"]=res[i].first;e["meta"]=hx(res[i].second);e["proof"]=std::vector<uint32_t>(ordered.begin()+i*256,ordered.begin()+(i+1)*256);entries.push_back(e);}});
    row["entries"]=entries;return row;
}
template<class T>std::shared_ptr<T> witness(){
    for(unsigned seed=0;seed<128;++seed){
        auto p=T::create();p->ksize=8;p->seed=addr(seed);skey_t key;key[31]=1;p->farmer_key=pubkey_t(key);
        if constexpr(std::is_same<T,ProofOfSpaceNFT>::value){p->contract=addr(12);p->plot_id=hash_t(std::string("MMX/PLOTID/NFT")+bytes_t<1>(&p->ksize,1)+p->seed+p->farmer_key+p->contract);}
        else p->plot_id=hash_t(std::string("MMX/PLOTID/OG")+bytes_t<1>(&p->ksize,1)+p->seed+p->farmer_key);
        p->difficulty=1;
        std::vector<uint32_t> xs,ys;std::vector<std::array<uint32_t,pos::N_META>> meta;std::mutex mutex;
        for(unsigned x=0;x<256;++x)pos::compute_f1(&xs,ys,meta,mutex,x,p->plot_id,8,0);
        std::vector<uint32_t> ordered;std::vector<std::pair<uint32_t,bytes_t<pos::META_BYTES_OUT>>> entries;
        try{entries=pos::candidate_full(xs,ys,meta,&ordered,p->plot_id,8);}catch(const std::exception&){continue;}
        for(size_t i=0;i<entries.size();++i){
            // Keep Y away from zero so default-filter challenge sampling has
            // its complete 16-value window on these small plots.
            if(entries[i].first<16)continue;
            p->proof_xs={ordered.begin()+i*256,ordered.begin()+(i+1)*256};std::vector<uint32_t> check;
            std::vector<std::pair<uint32_t,bytes_t<pos::META_BYTES_OUT>>> single;
            try{single=pos::compute(p->proof_xs,&check,p->plot_id,8,0);}catch(const std::exception&){continue;}
            if(single.size()!=1||check!=p->proof_xs)continue;
            for(uint64_t c=0;c<100000;++c){p->challenge=addr(c);auto challenge=get_plot_challenge(p->challenge,p->plot_id);if((challenge.template to_uint<uint32_t>()&255)==single[0].first){p->score=get_proof_score(p->calc_proof_hash());return p;}}
        }
    }
    throw std::runtime_error("could not construct a small native witness");
}
int main(int argc,char**argv){
    if(argc!=2)return 1;secp256k1_init();pos::set_remote_compute(false);output.open(argv[1]);
    for(unsigned k:{8,16,29,32})for(unsigned x:{0u,1u,123456789u,UINT32_MAX})for(unsigned n:{0,17}){
        const auto id=addr(n);std::vector<uint32_t> ys;std::vector<std::array<uint32_t,pos::N_META>> meta;std::mutex mutex;
        pos::compute_f1(nullptr,ys,meta,mutex,x,id,k,0);uint32_t msg[9]={};msg[0]=x;memcpy(msg+1,id.data(),32);hash_512_t key(msg,sizeof(msg));uint32_t mem[1024];bytes_t<128> hash;pos::gen_mem_array(mem,key.data(),1024);pos::calc_mem_hash(mem,hash.data(),256);
        vnx::Object row;row["kind"]="leaf";row["x"]=x;row["id"]=hx(id);row["ksize"]=k;row["y"]=ys[0];row["meta"]=std::vector<uint32_t>(meta[0].begin(),meta[0].end());row["key"]=hx(key);row["memory_hash"]=hx(hash);emit(row);
    }
    auto good=witness<ProofOfSpaceOG>();std::cout<<"Found native OG k8 witness, seed="<<hx(good->seed)<<std::endl;
    emit(computed(good->proof_xs,good->plot_id,8));
    for(unsigned k:{8,29,32}){std::vector<uint32_t> xs;for(unsigned x=0;x<256;++x)xs.push_back(k==8?0:x);emit(computed(xs,addr(0),k));}
    const auto pc=get_plot_challenge(good->challenge,good->plot_id);
    for(bool hf:{false,true})for(unsigned mutation=0;mutation<7;++mutation){
        auto xs=good->proof_xs;auto challenge=pc;unsigned post=0,k=8;
        if(mutation==1)xs.pop_back();if(mutation==2)std::reverse(xs.begin(),xs.end());if(mutation==3)challenge=addr(99999);if(mutation==4)post=256;if(mutation==5)k=7;if(mutation==6)xs[0]^=1;
        vnx::Object row;row["kind"]="verify";row["xs"]=xs;row["id"]=hx(good->plot_id);row["challenge"]=hx(challenge);row["ksize"]=k;row["plot_filter"]=0;row["post_filter"]=post;row["hardfork"]=hf;row["result"]=hash_outcome([&]{return pos::verify(xs,challenge,good->plot_id,0,post,k,hf);});emit(row);
    }
    auto params=ChainParams::create();params->min_ksize=8;params->plot_filter=0;params->post_filter=0;params->space_diff_constant=1;params->hardfork1_height=10;params->hardfork2_height=10;params->time_diff_constant=1;
    WorkReference node;node.params=params;node.challenge=good->challenge;
    for(unsigned height:{9,10})for(unsigned mutation=0;mutation<11;++mutation){
        auto p=std::make_shared<ProofOfSpaceOG>(*good);hash_t expected=good->challenge;uint64_t diff=1;
        if(mutation==1)diff=0;if(mutation==2)expected=addr(999);if(mutation==3)p->difficulty=2;if(mutation==4)p->plot_id=addr(999);if(mutation==5)p->ksize=0;if(mutation==6)p->proof_xs.pop_back();if(mutation==7)++p->score;if(mutation==8)std::reverse(p->proof_xs.begin(),p->proof_xs.end());
        auto par=std::make_shared<ChainParams>(*params);if(mutation==9)par->min_ksize=9;if(mutation==10)par->plot_filter=256;node.params=par;
        vnx::Object row;row["kind"]="proof";row["wire"]=wire(p);row["params"]=wire(par);row["height"]=height;row["challenge"]=hx(expected);row["difficulty"]=diff;row["result"]=outcome([&]{node.verify_proof(p,expected,diff,height);});emit(row);
    }
    node.params=params;
    auto nft=witness<ProofOfSpaceNFT>();std::cout<<"Found native NFT k8 witness, seed="<<hx(nft->seed)<<std::endl;
    for(unsigned height:{9,10}) {vnx::Object row;row["kind"]="proof";row["wire"]=wire(nft);row["params"]=wire(params);row["height"]=height;row["challenge"]=hx(nft->challenge);row["difficulty"]=1;row["result"]=outcome([&]{node.verify_proof(nft,nft->challenge,1,height);});emit(row);}
    auto filtered=[&](auto original){
        using T=typename decltype(original)::element_type;auto p=std::make_shared<T>(*original);auto par=std::make_shared<ChainParams>(*params);par->plot_filter=4;par->post_filter=10;
        const auto entry=pos::compute(p->proof_xs,nullptr,p->plot_id,8,0)[0];bool found=false;
        for(uint64_t c=0;c<10000000;++c){p->challenge=addr(c);if(!check_plot_filter(par,p->challenge,p->plot_id))continue;const hash_t challenge=get_plot_challenge(p->challenge,p->plot_id);const auto y=challenge.to_uint<uint32_t>()&255;if(entry.first<y||uint64_t(entry.first)>=uint64_t(y)+16)continue;if(!pos::check_post_filter(challenge,entry.second,10))continue;found=true;break;}
        if(!found)throw std::runtime_error("could not construct default-filter witness");p->score=get_proof_score(p->calc_proof_hash());node.params=par;
        vnx::Object row;row["kind"]="proof";row["wire"]=wire(p);row["params"]=wire(par);row["height"]=10;row["challenge"]=hx(p->challenge);row["difficulty"]=1;row["result"]=outcome([&]{node.verify_proof(p,p->challenge,1,10);});emit(row);
        row=vnx::Object();row["kind"]="verify";row["xs"]=p->proof_xs;row["id"]=hx(p->plot_id);row["challenge"]=hx(get_plot_challenge(p->challenge,p->plot_id));row["ksize"]=8;row["plot_filter"]=4;row["post_filter"]=10;row["hardfork"]=true;row["result"]=hash_outcome([&]{return pos::verify(p->proof_xs,get_plot_challenge(p->challenge,p->plot_id),p->plot_id,4,10,8,true);});emit(row);
    };
    filtered(good);filtered(nft);node.params=params;
    for(unsigned height:{9,10,11}){
        auto parent=BlockHeader::create();parent->hash=addr(700);parent->height=height-1;parent->vdf_height=height-1;parent->vdf_iters=100;parent->time_diff=1000;parent->space_diff=1;parent->challenge=addr(99);parent->proof_chain=addr(100);parent->space_fork_len=2;parent->space_fork_proofs=6;parent->total_weight=uint128(1)<<127;
        auto block=Block::create();block->height=height;block->prev=parent->hash;block->vdf_count=1;block->vdf_height=height;block->vdf_iters=101;block->time_diff=1000;block->proof={good};block->proof_hash=good->calc_proof_hash();block->proof_chain=calc_proof_chain(params,parent,block->proof_hash);bool fork=false;block->challenge=calc_next_challenge(params,parent,block,fork);block->is_space_fork=fork;block->set_space_diff(params,parent);block->weight=calc_block_weight(params,block,parent);block->total_weight=parent->total_weight+block->weight;
        node.parent=parent;node.iterations={1};node.challenge=good->challenge;
        for(unsigned mutation=0;mutation<3;++mutation) {
            auto b=std::make_shared<Block>(*block);parent->time_stamp=1000000;b->time_stamp=1010000;b->vdf_reward_addr={addr(8)};b->hash=b->calc_hash(params);skey_t key;key[31]=1;b->farmer_sig=signature_t::sign(key,b->hash);b->content_hash=b->calc_content_hash();
            if(mutation==1){b->farmer_sig=signature_t();b->content_hash=b->calc_content_hash();}
            if(mutation==2){b->weight+=1;b->total_weight+=1;b->hash=b->calc_hash(params);b->farmer_sig=signature_t::sign(key,b->hash);b->content_hash=b->calc_content_hash();}
            vnx::Object row;row["kind"]="pipeline";row["block"]=wire(b);row["params"]=wire(params);row["parent"]=wire(parent);row["iterations"]=node.iterations;row["challenge"]=hx(node.challenge);row["difficulty"]=node.difficulty;row["result"]=outcome([&]{if(!b->is_valid(params))throw std::logic_error("static validation failed");b->validate();node.verify_proof(b);});emit(row);
        }
        for(unsigned mutation=0;mutation<21;++mutation){
            auto b=std::make_shared<Block>(*block);
            if(mutation==1)b->vdf_count=101;if(mutation==2)++b->vdf_height;if(mutation==3)b->proof.clear();if(mutation==4)b->proof={good,good};if(mutation==5)b->proof_hash=addr(0);if(mutation==6)b->proof_chain=addr(0);if(mutation==7)++b->vdf_iters;if(mutation==8)b->time_diff=999;if(mutation==9)b->time_diff=1002;if(mutation==10)b->challenge=addr(0);if(mutation==11)b->is_space_fork=!fork;if(mutation==12)++b->space_diff;if(mutation==13)++b->space_fork_len;if(mutation==14)++b->space_fork_proofs;if(mutation==15)++b->weight;if(mutation==16)++b->total_weight;if(mutation==17)b->prev=addr(0);if(mutation==18)b->proof.resize(51,good);if(mutation==19){auto bad=std::make_shared<ProofOfSpaceOG>(*good);++bad->score;b->proof={bad};}if(mutation==20)b->time_diff=1001;
            vnx::Object row;row["kind"]="transition";row["block"]=wire(b);row["params"]=wire(params);row["parent"]=wire(parent);row["iterations"]=node.iterations;row["challenge"]=hx(node.challenge);row["difficulty"]=node.difficulty;row["result"]=outcome([&]{node.verify_proof(b);});emit(row);
        }
    }
    for(unsigned delay:{0,2,9})for(unsigned commit:{0,2,18})for(unsigned offset:{0,1,2,9,10,110})for(bool broken:{false,true}){
        HistoryReference history;history.params=std::make_shared<ChainParams>(*params);history.params->challenge_delay=delay;history.params->commit_delay=commit;history.params->infuse_delay=2;
        std::shared_ptr<BlockHeader> current;
        for(unsigned i=0;i<6;++i){auto h=BlockHeader::create();h->hash=addr(100+i);h->prev=addr(99+i);h->height=i;h->vdf_count=i%3+1;h->vdf_height=current?current->vdf_height+h->vdf_count:0;h->challenge=addr(200+i);h->space_diff=i+1;h->time_diff=1000*(i+1);history.headers[h->hash]=h;current=h;}
        if(broken)history.headers.erase(addr(102));
        uint64_t cd=0,iters=0;auto challenge=hash_outcome([&]{return history.get_challenge(current,offset,cd);});auto infusion=hash_outcome([&]{return history.get_infusion(current,offset,iters);});
        vnx::Object row;row["kind"]="history";row["params"]=wire(history.params);row["current"]=wire(current);row["offset"]=offset;row["challenge"]=challenge;row["difficulty"]=cd;row["infusion"]=infusion;row["iterations"]=iters;std::vector<std::string> headers;for(const auto& pair:history.headers)headers.push_back(wire(pair.second));row["headers"]=headers;emit(row);
        if(offset==0){
            vnx::Object ctx;ctx["kind"]="context";ctx["params"]=wire(history.params);ctx["current"]=wire(current);ctx["headers"]=headers;
            std::vector<uint64_t> iterations;uint64_t difficulty=0;hash_t challenge;
            ctx["result"]=outcome([&]{challenge=history.get_challenge(current,0,difficulty);auto parent=history.find_prev(current);if(!parent)throw std::logic_error("cannot verify");for(unsigned i=0;i<current->vdf_count;++i){uint64_t num=0;history.get_infusion(parent,i,num);iterations.push_back(num);}});
            ctx["challenge"]=hx(challenge);ctx["difficulty"]=difficulty;ctx["iterations"]=iterations;emit(ctx);
        }
    }
    for(uint64_t diff:{0ull,1ull,1023ull,1024ull,281474976710655ull,281474976710656ull,18446744073709551615ull})for(unsigned length:{0u,1u,48u,1000u,UINT32_MAX})for(unsigned proofs:{0u,1u,3u,144u,300u,UINT32_MAX}){
        auto parent=BlockHeader::create();parent->space_diff=diff;parent->space_fork_len=length;parent->space_fork_proofs=proofs;
        vnx::Object row;row["kind"]="space_diff";row["params"]=wire(params);row["parent"]=wire(parent);row["result"]=calc_new_space_diff(params,parent);emit(row);
    }
    for(unsigned height:{8u,9u,10u})for(unsigned count:{0u,1u,2u,100u})for(unsigned length:{0u,384u,385u,UINT32_MAX}){
        auto parent=BlockHeader::create();parent->height=height;parent->challenge=addr(123);parent->proof_chain=addr(456);parent->space_fork_len=length;
        auto b=BlockHeader::create();b->height=height+1;b->vdf_count=count;b->proof_hash=addr(17);b->proof_chain=calc_proof_chain(params,parent,b->proof_hash);bool fork=false;auto challenge=calc_next_challenge(params,parent,b,fork);
        vnx::Object row;row["kind"]="challenge";row["params"]=wire(params);row["parent"]=wire(parent);row["height"]=b->height;row["count"]=count;row["proof_hash"]=hx(b->proof_hash);row["chain"]=hx(b->proof_chain);row["challenge"]=hx(challenge);row["fork"]=fork;emit(row);
    }
    for(unsigned k:{8u,29u,32u})for(uint64_t diff:{0ull,1ull,1000000000000ull})for(bool hf:{false,true})for(unsigned shape:{0u,1u,2u}){
        auto p=std::make_shared<ChainParams>(*params);p->space_diff_constant=100000000;p->post_filter=10;
        uint256_t v=shape==0?uint256_t():shape==1?uint256_t(-1):(uint256_t(1)<<255);hash_t quality;quality.from_uint(v);
        vnx::Object row;row["kind"]="threshold";row["params"]=wire(p);row["ksize"]=k;row["difficulty"]=diff;row["hardfork"]=hf;row["quality"]=hx(quality);row["result"]=check_proof_threshold(p,k,quality,diff,hf);emit(row);
    }
    auto par=std::make_shared<ChainParams>(*params);par->space_diff_constant=100000000;par->post_filter=10;
    for(bool hf:{false,true})for(int delta:{-1,0,1}){
        const uint64_t diff=1000000000000ull;const unsigned k=29;const auto threshold=((uint256_t(1)<<255)/(uint128_t(diff)*par->space_diff_constant))*(2*k+1);uint256_t v=threshold<<(k-1+(hf?par->post_filter:0));if(delta<0)--v;if(delta>0)++v;hash_t quality;quality.from_uint(v);
        vnx::Object row;row["kind"]="threshold";row["params"]=wire(par);row["ksize"]=k;row["difficulty"]=diff;row["hardfork"]=hf;row["quality"]=hx(quality);row["result"]=check_proof_threshold(par,k,quality,diff,hf);emit(row);
    }
    const auto entry=pos::compute(good->proof_xs,nullptr,good->plot_id,8,0)[0];
    for(unsigned bits:{0u,1u,4u,10u,255u,256u})for(unsigned nonce:{0u,1u,999u}){
        const auto challenge=addr(nonce);vnx::Object row;row["kind"]="filters";row["bits"]=bits;row["challenge"]=hx(challenge);row["id"]=hx(good->plot_id);row["meta"]=hx(entry.second);row["plot_challenge"]=hx(get_plot_challenge(challenge,good->plot_id));row["quality"]=hx(pos::calc_quality(challenge,entry.second));auto p=std::make_shared<ChainParams>(*params);p->plot_filter=bits;row["plot_filter"]=check_plot_filter(p,challenge,good->plot_id);row["post_filter"]=pos::check_post_filter(challenge,entry.second,bits);emit(row);
    }
    std::cout<<"Generated "<<count<<" native work comparisons\n";
    return output?0:1;
}
