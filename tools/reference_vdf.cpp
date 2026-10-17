#include <mmx/Block.hxx>
#include <mmx/ProofOfTime.hxx>
#include <mmx/ProofOfSpaceOG.hxx>
#include <mmx/ProofOfSpaceNFT.hxx>
#include <mmx/pos/verify.h>
#include <mmx/VDF_Point.hxx>
#include <mmx/write_bytes.h>
#include <mmx/utils.h>
#include <vnx/vnx.h>
#include <sha256_avx2.h>
#include <sha256_ni.h>
#include <sha256_arm.h>
#include <atomic>
#include <fstream>
#include <iomanip>
#include <sstream>
namespace mmx {
struct VdfReference {
    std::shared_ptr<ChainParams> params;
    std::map<hash_t,std::shared_ptr<BlockHeader>> headers;
    std::multimap<hash_t,std::shared_ptr<const VDF_Point>> vdf_tree;
    std::shared_ptr<const BlockHeader> get_header(const hash_t& h)const {auto i=headers.find(h);return i==headers.end()?nullptr:i->second;}
    std::shared_ptr<const BlockHeader> find_prev(std::shared_ptr<const BlockHeader>,size_t distance=1,bool clamped=false)const;
    uint64_t get_time_diff(std::shared_ptr<const BlockHeader>)const;
    bool find_infusion(std::shared_ptr<const BlockHeader>,uint32_t,hash_t&,uint64_t&)const;
    hash_t get_infusion(std::shared_ptr<const BlockHeader>,uint32_t,uint64_t&)const;
    bool find_challenge(std::shared_ptr<const BlockHeader>,uint32_t,hash_t&,uint64_t&)const;
    hash_t get_challenge(std::shared_ptr<const BlockHeader>,uint32_t,uint64_t&)const;
    void validate_diff_adjust(const uint64_t&,const uint64_t&)const;
    void verify_proof(std::shared_ptr<const BlockHeader>)const;
    void verify_proof(std::shared_ptr<const ProofOfSpace>,const hash_t&,uint64_t,const uint32_t&)const;
    template<class T>void verify_proof_impl(std::shared_ptr<const T>,const hash_t&,uint64_t,const uint32_t&)const;
    void verify_vdf(std::shared_ptr<const ProofOfTime>,int64_t recv_time=0);
    void verify_vdf_cpu(std::shared_ptr<const ProofOfTime>)const;
    void verify_vdf_fallback(std::shared_ptr<const ProofOfTime>)const;
    std::vector<std::shared_ptr<const VDF_Point>> find_vdf_points(std::shared_ptr<const BlockHeader>)const;
};
// MMX_VDF_METHODS
struct ProofOfTimeCapture : ProofOfTime {std::vector<uint8_t> capture()const;};
struct VDF_PointCapture : VDF_Point {std::vector<uint8_t> capture()const;};
// MMX_VDF_BYTES
}
using namespace mmx;
std::string hex(const uint8_t* p,size_t n){std::ostringstream s;s<<std::hex<<std::setfill('0');for(size_t i=0;i<n;++i)s<<std::setw(2)<<unsigned(p[i]);return s.str();}
template<size_t N>std::string hx(const bytes_t<N>& p){return hex(p.data(),N);}
hash_t addr(uint64_t n){hash_t h;h.from_uint(n);return h;}
skey_t secret(){skey_t s;s[31]=1;return s;}
template<class F>std::string outcome(F f){try{f();return "OK";}catch(const std::exception&e){return e.what();}}
std::string wire(std::shared_ptr<const vnx::Value> v){std::vector<uint8_t>b;vnx::VectorOutputStream s(&b);vnx::TypeOutput out(&s);vnx::write(out,v);out.flush();return hex(b.data(),b.size());}
template<class T>std::shared_ptr<T> decode(const std::string& text){const auto raw=vnx::from_hex_string(text);vnx::VectorInputStream stream(&raw);vnx::TypeInput in(&stream);std::shared_ptr<T> out;vnx::read(in,out);return out;}
std::ofstream output;size_t count=0;
std::map<std::pair<std::string,std::string>,std::string> snapshots;
void emit(vnx::Object row){
    const auto key=std::make_pair(row["params"].to<std::string>(),vnx::to_string(row["headers"]));const auto old=snapshots.find(key);
    if(old==snapshots.end())snapshots[key]=row["name"].to<std::string>();
    else {row["seed"]=old->second;row.field.erase("params");row.field.erase("headers");}
    output<<vnx::to_string(row)<<'\n';++count;
}
std::vector<std::string> headers(const VdfReference& node){std::vector<std::string> out;for(const auto& i:node.headers)out.push_back(wire(i.second));return out;}
void seal(std::shared_ptr<ProofOfTime> p){p->hash=p->calc_hash();p->timelord_key=pubkey_t(secret());p->timelord_sig=signature_t::sign(secret(),p->hash);p->content_hash=p->calc_content_hash();}
void compute(std::shared_ptr<ProofOfTime> p,unsigned count){p->segments.clear();hash_t value=hash_t(p->input+p->prev);value=hash_t(value+p->reward_addr);for(unsigned i=0;i<count;++i){for(unsigned j=0;j<p->segment_size;++j)value=hash_t(value.data(),32);p->segments.push_back(value);}seal(p);}
auto point(std::shared_ptr<const ProofOfTime> p){auto v=VDF_Point::create();v->vdf_height=p->vdf_height;v->start=p->start;v->num_iters=p->num_iters;v->input=p->input;v->output=p->get_output();v->prev=p->prev;v->reward_addr=p->reward_addr;v->proof=p;v->content_hash=v->calc_hash();return v;}
void emit_proof(const std::string& name,std::shared_ptr<ProofOfTime> p,VdfReference& node){
    vnx::Object row;row["kind"]="proof";row["name"]=name;row["wire"]=wire(p);row["params"]=wire(node.params);row["headers"]=headers(node);
    row["hash"]=hx(p->calc_hash());row["content_hash"]=hx(p->calc_content_hash());row["valid"]=p->is_valid();row["output"]=hx(p->get_output());row["end_iters"]=p->get_vdf_iters();
    ProofOfTimeCapture capture;static_cast<ProofOfTime&>(capture)=*p;const auto preimage=capture.capture();row["serialized"]=hex(preimage.data(),preimage.size());
    row["auth"]=outcome([&]{p->validate();});row["admission"]=outcome([&]{node.verify_vdf(p);});row["cpu"]=outcome([&]{node.verify_vdf_cpu(p);});row["fallback"]=outcome([&]{node.verify_vdf_fallback(p);});
    auto v=point(p);v->recv_time=-123;row["point"]=wire(v);row["point_hash"]=hx(v->calc_hash());VDF_PointCapture vc;static_cast<VDF_Point&>(vc)=*v;const auto vb=vc.capture();row["point_serialized"]=hex(vb.data(),vb.size());emit(row);
}
auto chain(VdfReference& node,unsigned height){std::shared_ptr<BlockHeader> last;for(unsigned i=0;i<=height;++i){auto h=BlockHeader::create();h->height=i;h->hash=addr(1000+i);h->prev=last?last->hash:hash_t();h->vdf_height=i*2;h->vdf_output=addr(2000+i);h->time_diff=20+i*10;h->vdf_iters=100+i*10;node.headers[h->hash]=h;last=h;}return last;}
int main(int argc,char**argv){
    if(argc!=3)return 1;secp256k1_init();pos::set_remote_compute(false);output.open(argv[1]);
    for(unsigned size:{0,1,2,3,7,31})for(unsigned n:{0,1,2,15,16,17,18,32,34}){
        VdfReference node;node.params=ChainParams::create();node.params->time_diff_divider=1;node.params->time_diff_constant=1;node.params->vdf_segment_size=size;
        auto prev=chain(node,0);prev->time_diff=uint64_t(n)*size;
        auto p=ProofOfTime::create();p->vdf_height=1;p->start=UINT64_MAX-2;p->num_iters=prev->time_diff;p->segment_size=size;p->input=addr(1);p->prev=prev->hash;p->reward_addr=addr(2);compute(p,n);
        const auto label="size"+std::to_string(size)+"_count"+std::to_string(n);emit_proof(label,p,node);
        if(n){for(unsigned index:std::set<unsigned>{0,n/2,n-1}){auto q=std::make_shared<ProofOfTime>(*p);q->segments[index][0]^=1;seal(q);emit_proof(label+"_bad_segment"+std::to_string(index),q,node);}}
    }
    {VdfReference ctx;ctx.params=ChainParams::create();ctx.params->time_diff_divider=1;ctx.params->time_diff_constant=1;auto h=chain(ctx,0);h->time_diff=2*ctx.params->vdf_segment_size;auto q=ProofOfTime::create();q->prev=h->hash;q->input=addr(1);q->reward_addr=addr(2);q->vdf_height=42;q->segment_size=ctx.params->vdf_segment_size;q->num_iters=h->time_diff;compute(q,2);emit_proof("production_segment_size",q,ctx);q->segments.back()[0]^=1;seal(q);emit_proof("production_segment_corrupt",q,ctx);}
    {VdfReference ctx;ctx.params=ChainParams::create();ctx.params->time_diff_divider=1;ctx.params->time_diff_constant=(uint64_t(1)<<63)+1;ctx.params->vdf_segment_size=1;auto h=chain(ctx,0);h->time_diff=2;auto q=ProofOfTime::create();q->prev=h->hash;q->input=addr(1);q->reward_addr=addr(2);q->vdf_height=1;q->segment_size=1;q->num_iters=get_block_iters(ctx.params,h->time_diff);compute(q,2);emit_proof("iteration_product_wrap",q,ctx);}
    VdfReference node;node.params=ChainParams::create();node.params->time_diff_divider=10;node.params->time_diff_constant=2;node.params->vdf_segment_size=2;auto prev=chain(node,4);
    auto p=ProofOfTime::create();p->vdf_height=UINT32_MAX;p->start=UINT64_MAX;p->segment_size=2;p->num_iters=get_block_iters(node.params,node.get_time_diff(prev));p->input=addr(1);p->prev=prev->hash;p->reward_addr=addr(2);compute(p,p->num_iters/2);
    emit_proof("admission_valid",p,node);
    auto change=[&](std::string label,auto edit,bool reseal=true){auto q=std::make_shared<ProofOfTime>(*p);edit(*q);if(reseal)seal(q);emit_proof(label,q,node);};
    change("version",[](auto&q){q.version=1;});change("height_zero",[](auto&q){q.vdf_height=0;});change("iters_zero",[](auto&q){q.num_iters=0;});change("num_iters",[](auto&q){++q.num_iters;});change("segment_size",[](auto&q){++q.segment_size;});change("segment_count",[](auto&q){q.segments.pop_back();});
    change("missing_infusion",[](auto&q){q.prev=addr(42);});change("input",[](auto&q){q.input=addr(43);});change("reward",[](auto&q){q.reward_addr=addr(44);});change("empty_segments",[](auto&q){q.segments.clear();});
    change("bad_hash",[](auto&q){q.hash[0]^=1;q.content_hash=q.calc_content_hash();},false);change("bad_content_hash",[](auto&q){q.content_hash[0]^=1;},false);
    change("bad_signature",[](auto&q){q.timelord_sig=signature_t();q.content_hash=q.calc_content_hash();},false);change("bad_key",[](auto&q){q.timelord_key=pubkey_t();q.content_hash=q.calc_content_hash();},false);
    change("wrong_signer",[](auto&q){skey_t key;key[31]=2;q.timelord_sig=signature_t::sign(key,q.hash);q.content_hash=q.calc_content_hash();},false);
    auto incomplete=node;incomplete.headers.erase(addr(1001));emit_proof("missing_time_history",p,incomplete);
    for(unsigned delay:{0,1,2,5,100}){auto ctx=node;ctx.params=std::make_shared<ChainParams>(*node.params);ctx.params->commit_delay=delay;auto q=std::make_shared<ProofOfTime>(*p);q->num_iters=get_block_iters(ctx.params,ctx.get_time_diff(prev));compute(q,q->num_iters/2);emit_proof("commit_delay"+std::to_string(delay),q,ctx);}
    for(unsigned infuse:{0,1,3,8})for(unsigned commit:{0,2})for(unsigned n:{1,2,4}){
        VdfReference ctx;ctx.params=std::make_shared<ChainParams>(*node.params);ctx.params->infuse_delay=infuse;ctx.params->commit_delay=commit;auto parent=chain(ctx,4);std::vector<std::shared_ptr<ProofOfTime>> proofs;
        hash_t input=parent->vdf_output;
        for(unsigned i=0;i<n;++i){auto q=ProofOfTime::create();q->prev=ctx.get_infusion(parent,i,q->num_iters);q->segment_size=2;q->input=input;q->reward_addr=addr(300+i);q->start=UINT64_MAX-i;q->vdf_height=400+i;compute(q,q->num_iters/2);ctx.verify_vdf(q);ctx.verify_vdf_cpu(q);proofs.push_back(q);auto v=point(q);ctx.vdf_tree.emplace(v->output,v);input=v->output;}
        auto b=BlockHeader::create();b->prev=parent->hash;b->vdf_count=n;b->vdf_output=input;b->vdf_height=parent->vdf_height+n;b->vdf_reward_addr.resize(n,addr(999));
        auto emit_link=[&](std::string label,VdfReference& ref,std::shared_ptr<BlockHeader> block,std::vector<std::shared_ptr<ProofOfTime>> pool){vnx::Object row;row["kind"]="link";row["name"]="i"+std::to_string(infuse)+"_c"+std::to_string(commit)+"_n"+std::to_string(n)+"_"+label;row["params"]=wire(ref.params);row["headers"]=headers(ref);row["block"]=wire(block);std::vector<std::string> ps,chosen;for(const auto&q:pool)ps.push_back(wire(q));row["proofs"]=ps;for(const auto&v:ref.find_vdf_points(block))chosen.push_back(hx(v->content_hash));row["chosen"]=chosen;emit(row);};
        emit_link("valid_metadata_exemptions",ctx,b,proofs);
        {auto bad=std::make_shared<BlockHeader>(*b);bad->vdf_output=addr(7);emit_link("output",ctx,bad,proofs);}
        {auto bad=std::make_shared<BlockHeader>(*b);bad->prev=addr(7);emit_link("parent",ctx,bad,proofs);}
        {auto bad=std::make_shared<BlockHeader>(*b);++bad->vdf_count;emit_link("count",ctx,bad,proofs);}
        {auto ref=ctx;ref.vdf_tree.clear();emit_link("empty_pool",ref,b,{});}
        if(n>1){auto ref=ctx;auto removed=proofs;removed.erase(removed.begin());ref.vdf_tree.clear();for(const auto&q:removed){auto v=point(q);ref.vdf_tree.emplace(v->output,v);}emit_link("missing_first",ref,b,removed);}
        {auto ref=ctx;auto pool=proofs;auto alt=std::make_shared<ProofOfTime>(*proofs.back());alt->start=17;alt->vdf_height=18;seal(alt);auto v=point(alt);ref.vdf_tree.emplace(v->output,v);pool.push_back(alt);emit_link("duplicate_output",ref,b,pool);}
        {auto ref=ctx;auto pool=proofs;std::reverse(pool.begin(),pool.end());emit_link("reversed_pool",ref,b,pool);}
    }
    std::ifstream work(argv[2]);std::string line;
    while(std::getline(work,line)){
        const auto seed=vnx::from_string<vnx::Object>(line);if(seed["kind"].to<std::string>()!="pipeline"||seed["result"].to<std::string>()!="OK")continue;
        for(unsigned n:{1,2,4}){
            VdfReference ctx;ctx.params=decode<ChainParams>(seed["params"].to<std::string>());ctx.params->challenge_delay=n;ctx.params->infuse_delay=0;ctx.params->commit_delay=0;ctx.params->time_diff_constant=2;ctx.params->vdf_segment_size=1;
            auto parent=decode<BlockHeader>(seed["parent"].to<std::string>());auto b=decode<Block>(seed["block"].to<std::string>());parent->challenge=b->proof[0]->challenge;parent->vdf_output=addr(1234);
            auto last=parent;for(unsigned i=0;i<n;++i){auto ancestor=std::make_shared<BlockHeader>(*parent);ancestor->height=parent->height-i-1;ancestor->hash=addr(900+i);ancestor->vdf_height=parent->vdf_height-i-1;last->prev=ancestor->hash;ctx.headers[ancestor->hash]=ancestor;last=ancestor;}
            ctx.headers[parent->hash]=parent;b->vdf_count=n;b->vdf_height=parent->vdf_height+n;b->vdf_iters=parent->vdf_iters;b->time_stamp=parent->time_stamp+n*ctx.params->block_interval_ms;
            std::vector<std::shared_ptr<ProofOfTime>> proofs;hash_t input=parent->vdf_output;b->vdf_reward_addr.clear();
            for(unsigned i=0;i<n;++i){auto q=ProofOfTime::create();q->prev=ctx.get_infusion(parent,i,q->num_iters);q->segment_size=1;q->input=input;q->reward_addr=addr(300+i);q->start=UINT64_MAX-i;q->vdf_height=400+i;compute(q,q->num_iters);ctx.verify_vdf(q);ctx.verify_vdf_cpu(q);proofs.push_back(q);auto v=point(q);ctx.vdf_tree.emplace(v->output,v);input=v->output;b->vdf_reward_addr.push_back(q->reward_addr);b->vdf_iters+=q->num_iters;}
            b->vdf_output=input;b->proof_chain=calc_proof_chain(ctx.params,parent,b->proof_hash);bool fork=false;b->challenge=calc_next_challenge(ctx.params,parent,b,fork);b->is_space_fork=fork;b->set_space_diff(ctx.params,parent);b->weight=calc_block_weight(ctx.params,b,parent);b->total_weight=parent->total_weight+b->weight;
            for(unsigned mutation=0;mutation<7;++mutation){auto candidate=std::make_shared<Block>(*b);auto ref=ctx;auto pool=proofs;
                if(mutation==1)candidate->vdf_output=addr(7);if(mutation==2){ref.vdf_tree.clear();pool.clear();}if(mutation==3){candidate->weight+=1;candidate->total_weight+=1;}if(mutation==4)++candidate->vdf_iters;if(mutation==5)candidate->vdf_reward_addr.assign(n,addr(999));
                candidate->hash=candidate->calc_hash(ctx.params);candidate->farmer_sig=signature_t::sign(secret(),candidate->hash);if(mutation==6)candidate->farmer_sig=signature_t();candidate->content_hash=candidate->calc_content_hash();
                vnx::Object row;row["kind"]="pipeline";row["name"]="h"+std::to_string(b->height)+"_n"+std::to_string(n)+"_m"+std::to_string(mutation);row["block"]=wire(candidate);row["params"]=wire(ref.params);row["parent"]=wire(parent);row["headers"]=headers(ref);std::vector<std::string> ps;for(const auto&q:pool)ps.push_back(wire(q));row["proofs"]=ps;
                row["result"]=outcome([&]{if(!candidate->is_valid(ref.params))throw std::logic_error("static validation failed");candidate->validate();if(ref.find_vdf_points(candidate).empty())throw std::logic_error("cannot find VDF points");ref.verify_proof(candidate);});emit(row);
            }
        }
    }
    std::cerr<<"wrote "<<count<<" native VDF cases\n";
}
