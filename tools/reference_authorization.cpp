// Native contract authentication and Node's signature-selection / lookup rules.
// Fixture generation only; no wallet, running node, or chain data is used.
#include <mmx/Transaction.hxx>
#include <mmx/contract/PubKey.hxx>
#include <mmx/contract/MultiSig.hxx>
#include <mmx/contract/Executable.hxx>
#include <mmx/contract/Binary.hxx>
#include <mmx/contract/TokenBase.hxx>
#include <mmx/contract/Data.hxx>
#include <mmx/contract/WebData.hxx>
#include <mmx/operation/Deposit.hxx>
#include <mmx/solution/PubKey.hxx>
#include <mmx/solution/MultiSig.hxx>
#include <mmx/exception.h>
#include <mmx/secp256k1.hpp>
#include <vnx/vnx.h>
#include <fstream>
#include <iomanip>
#include <sstream>
using namespace mmx;
std::ofstream auth, crypto;
std::string hex(const uint8_t* data,size_t size){std::ostringstream s;s<<std::hex<<std::setfill('0');for(size_t i=0;i<size;++i)s<<std::setw(2)<<unsigned(data[i]);return s.str();}
template<size_t N> std::string hx(const bytes_t<N>& value){return hex(value.data(),N);}
std::string wire(std::shared_ptr<const vnx::Value> value){if(!value)return "-";std::vector<uint8_t>b;vnx::VectorOutputStream s(&b);vnx::TypeOutput out(&s);vnx::write(out,value);out.flush();return hex(b.data(),b.size());}
addr_t addr(uint64_t n){addr_t a;a.from_uint(n);return a;}
skey_t secret(unsigned n){skey_t k;k[30]=n>>8;k[31]=n;return k;}
auto signed_solution(unsigned n,const hash_t& txid){auto s=solution::PubKey::create();s->pubkey=pubkey_t(secret(n));s->signature=signature_t::sign(secret(n),txid);return s;}
auto pub(const addr_t& address){auto p=contract::PubKey::create();p->address=address;return p;}
void emit_crypto(const std::string& name,const hash_t& message,const pubkey_t& key,const signature_t& signature){
    std::string result;
    try{result=signature.verify(key,message)?"1":"0";}catch(const std::exception& e){result=std::string(e.what()).find("signature_parse")!=std::string::npos?"SIG":"KEY";}
    crypto<<name<<'\t'<<hx(message)<<'\t'<<hx(key)<<'\t'<<hx(signature)<<'\t'<<hx(key.get_addr())<<'\t'<<result<<'\n';
}
std::pair<std::string,std::string> outcome(const std::function<void()>& fn){try{fn();return {"ok","-"};}catch(const static_failure& e){return {"static",e.what()};}catch(const std::exception& e){return {"dynamic",e.what()};}}
void emit_contract(const std::string& name,std::shared_ptr<Contract> c,std::shared_ptr<Solution> s,const hash_t& txid,unsigned version){
    const auto result=outcome([&]{c->validate(s,txid,version);});
    auth<<name<<"\tcontract\t"<<version<<'\t'<<hx(txid)<<'\t'<<wire(c)<<'\t'<<wire(s)<<"\t-\t0\t0\t0\t-\t"<<result.first<<'\t'<<result.second<<'\n';
}
auto multi(const std::set<addr_t>& owners,unsigned required){auto c=contract::MultiSig::create();c->owners=owners;c->num_required=required;return c;}
struct Spec{std::string name,mode="sender";std::shared_ptr<Transaction> tx;std::shared_ptr<Contract> stored;uint32_t index=0;uint64_t cost=0,read_cost=17,limit=1000000;};
void emit_step(Spec s){
    auto tx=s.tx;uint64_t cost=s.cost;std::vector<addr_t> lookups;
    auto lookup=[&](const addr_t& a){lookups.push_back(a);const uint64_t fee=1000+s.read_cost;if(cost+fee>s.limit)throw std::runtime_error("not enough gas to read contract");cost+=fee;return s.stored;};
    auto result=outcome([&]{
        if(s.mode=="sender"){
            if(!tx->sender)throw static_failure("missing transaction sender");
            if(tx->solutions.empty())throw static_failure("missing sender signature");
            pub(*tx->sender)->validate(tx->solutions[0],tx->id,tx->version);
        }else if(s.mode=="input"){
            const auto& in=tx->inputs.at(s.index);auto sol=tx->get_solution(in.solution);
            if(!sol)throw invalid_solution("missing solution");
            const auto c=(in.flags&txin_t::IS_EXEC)?lookup(in.address):pub(in.address);
            if(!c)throw std::logic_error("no such contract");c->validate(sol,tx->id,tx->version);
        }else{
            auto op=std::dynamic_pointer_cast<const operation::Execute>(tx->execute.at(s.index));
            if(!op)throw std::logic_error("invalid operation");
            if(op->user){auto c=lookup(*op->user);if(!c)c=pub(*op->user);c->validate(tx->get_solution(op->solution),tx->id,tx->version);}
        }
    });
    std::string calls;for(const auto& a:lookups){if(!calls.empty())calls+=',';calls+=hx(a);}
    auth<<s.name<<'\t'<<s.mode<<'\t'<<tx->version<<'\t'<<hx(tx->id)<<'\t'<<wire(tx)<<'\t'<<wire(s.stored)<<'\t'<<s.index<<'\t'<<s.cost<<'\t'<<s.limit<<'\t'<<cost<<'\t'<<(calls.empty()?"-":calls)<<'\t'<<result.first<<'\t'<<result.second<<'\n';
}
int main(int argc,char** argv){
    secp256k1_init();vnx::init("reference_authorization",argc,argv);crypto.open(argv[1]);auth.open(argv[2]);
    emit_crypto("zero_initial",hash_t(),pubkey_t(),signature_t());
    const auto target=uint64_t(vnx::Hash64(signature_t().crc64(),vnx::Hash64(0)))%16384;
    bool collision=false;
    for(uint64_t n=1;n<1000000;++n){hash_t msg;msg.from_uint(n);auto s=signed_solution(1,msg);if(uint64_t(vnx::Hash64(s->signature.crc64(),vnx::Hash64(0)))%16384==target){emit_crypto("evict_zero",msg,s->pubkey,s->signature);collision=true;break;}}
    if(!collision)return 1;
    emit_crypto("zero_evicted",hash_t(),pubkey_t(),signature_t());
    hash_t message;message[0]=42;auto a=signed_solution(1,message),b=signed_solution(2,message),c=signed_solution(3,message),outsider=signed_solution(4,message);
    for(unsigned k=1;k<=100;++k){hash_t msg;msg.from_uint(k*7919);auto s=signed_solution(k,msg);emit_crypto("valid_"+std::to_string(k),msg,s->pubkey,s->signature);msg[31]^=1;emit_crypto("wrong_message_"+std::to_string(k),msg,s->pubkey,s->signature);s->signature[0]^=1;emit_crypto("wrong_signature_"+std::to_string(k),msg,s->pubkey,s->signature);s->pubkey[0]=4;emit_crypto("bad_key_"+std::to_string(k),msg,s->pubkey,s->signature);}
    const bytes_t<32> order("FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEBAAEDCE6AF48A03BBFD25E8CD0364141");
    for(unsigned part:{0,1})for(unsigned offset:{0,1,2}){signature_t sig=a->signature;for(unsigned i=0;i<32;++i)sig[part*32+i]=order[i];if(offset==1)--sig[part*32+31];if(offset==2)++sig[part*32+31];emit_crypto("scalar_"+std::to_string(part)+"_"+std::to_string(offset),message,a->pubkey,sig);emit_crypto("scalar_bad_key_"+std::to_string(part)+"_"+std::to_string(offset),message,pubkey_t(),sig);}
    emit_crypto("zero_scalars_valid_key",message,a->pubkey,signature_t());emit_crypto("zero_scalars_bad_key",message,pubkey_t(),signature_t());
    auto high=std::make_shared<solution::PubKey>(*a);unsigned borrow=0;for(int i=31;i>=0;--i){const int v=int(order[i])-int(a->signature[32+i])-int(borrow);high->signature[32+i]=v;borrow=v<0;}emit_crypto("high_s",message,high->pubkey,high->signature);
    auto wrong=std::make_shared<solution::PubKey>(*a);wrong->signature[8]^=1;
    auto badkey=std::make_shared<solution::PubKey>(*a);badkey->pubkey=pubkey_t();
    auto overflow=std::make_shared<solution::PubKey>(*a);for(unsigned i=0;i<32;++i)overflow->signature[i]=order[i];
    auto zerokey=solution::PubKey::create();
    std::vector<std::pair<std::string,std::shared_ptr<Solution>>> samples={{"valid",a},{"other_key",b},{"wrong_sig",wrong},{"high_s",high},{"bad_key",badkey},{"overflow",overflow},{"zero",zerokey},{"missing",nullptr},{"nested",solution::MultiSig::create()},{"base_solution",Solution::create()}};
    for(unsigned v:{0,1,2}){
        for(const auto& s:samples)emit_contract("pub_"+s.first+"_v"+std::to_string(v),pub(a->pubkey.get_addr()),s.second,message,v);
        emit_contract("pub_bad_key_matching_address_v"+std::to_string(v),pub(badkey->pubkey.get_addr()),badkey,message,v);
        emit_contract("pub_overflow_matching_address_v"+std::to_string(v),pub(overflow->pubkey.get_addr()),overflow,message,v);
        for(unsigned required:{0,1,2,3,4}){
            auto m=multi({a->pubkey.get_addr(),b->pubkey.get_addr(),c->pubkey.get_addr()},required);
            for(const auto& s:samples)emit_contract("multi_direct_"+s.first+"_r"+std::to_string(required)+"_v"+std::to_string(v),m,s.second,message,v);
            for(unsigned declared:{0,1,2,3,4})for(unsigned shape=0;shape<12;++shape){
                auto sol=solution::MultiSig::create();sol->num_required=declared;
                if(shape&1)sol->solutions[a->pubkey.get_addr()]=a;
                if(shape&2)sol->solutions[b->pubkey.get_addr()]=b;
                if(shape&4)sol->solutions[c->pubkey.get_addr()]=c;
                if(shape==8){sol->solutions[a->pubkey.get_addr()]=a;sol->solutions[b->pubkey.get_addr()]=nullptr;}
                if(shape==9){sol->solutions[a->pubkey.get_addr()]=a;sol->solutions[b->pubkey.get_addr()]=solution::MultiSig::create();}
                if(shape==10){sol->solutions[a->pubkey.get_addr()]=a;sol->solutions[outsider->pubkey.get_addr()]=outsider;}
                if(shape==11){sol->solutions[a->pubkey.get_addr()]=a;sol->solutions[b->pubkey.get_addr()]=wrong;}
                emit_contract("multi_r"+std::to_string(required)+"_d"+std::to_string(declared)+"_s"+std::to_string(shape)+"_v"+std::to_string(v),m,sol,message,v);
            }
        }
        for(const auto& unsupported:std::vector<std::shared_ptr<Contract>>{Contract::create(),contract::Data::create(),contract::WebData::create(),contract::TokenBase::create(),contract::Executable::create(),contract::Binary::create()})emit_contract("unsupported_"+unsupported->get_type_name()+"_v"+std::to_string(v),unsupported,a,message,v);
    }
    // Do not stop after meeting the threshold: a later owner must still verify.
    auto late=solution::MultiSig::create();late->num_required=1;late->solutions[a->pubkey.get_addr()]=a;late->solutions[b->pubkey.get_addr()]=wrong;
    emit_contract("bad_owner_after_threshold",multi({a->pubkey.get_addr(),b->pubkey.get_addr()},1),late,message,1);
    auto mismatch=solution::MultiSig::create();mismatch->num_required=1;mismatch->solutions[a->pubkey.get_addr()]=b;
    emit_contract("owner_pubkey_mismatch",multi({a->pubkey.get_addr()},1),mismatch,message,1);
    auto invalidsig=solution::MultiSig::create();invalidsig->num_required=1;invalidsig->solutions[a->pubkey.get_addr()]=wrong;
    emit_contract("owner_invalid_signature",multi({a->pubkey.get_addr()},1),invalidsig,message,1);
    auto unknown=solution::MultiSig::create();unknown->num_required=1;unknown->solutions[a->pubkey.get_addr()]=a;unknown->solutions[outsider->pubkey.get_addr()]=nullptr;
    for(unsigned v:{0,1})emit_contract("non_owner_null_v"+std::to_string(v),multi({a->pubkey.get_addr()},1),unknown,message,v);
    emit_contract("zero_key_evicted",pub(zerokey->pubkey.get_addr()),zerokey,hash_t(),1);
    for(unsigned v:{0,1}){
        auto tx=Transaction::create();tx->id=message;tx->version=v;tx->sender=a->pubkey.get_addr();tx->solutions={a};
        emit_step({"sender_valid_v"+std::to_string(v),"sender",tx});
        auto clone=[&](){return std::make_shared<Transaction>(*tx);};
        auto q=clone();q->sender=nullptr;emit_step({"sender_missing_v"+std::to_string(v),"sender",q});
        q=clone();q->solutions.clear();emit_step({"sender_no_solutions_v"+std::to_string(v),"sender",q});
        q=clone();q->solutions={nullptr};emit_step({"sender_null_solution_v"+std::to_string(v),"sender",q});
        q=clone();q->solutions={b};emit_step({"sender_wrong_key_v"+std::to_string(v),"sender",q});
        q=clone();q->solutions={wrong};emit_step({"sender_wrong_signature_v"+std::to_string(v),"sender",q});
        q=clone();q->solutions={high};emit_step({"sender_high_s_v"+std::to_string(v),"sender",q});
        q=clone();q->solutions={overflow};emit_step({"sender_parse_error_v"+std::to_string(v),"sender",q});
        q=clone();auto sol=solution::MultiSig::create();sol->solutions[a->pubkey.get_addr()]=a;sol->num_required=1;q->solutions={sol};emit_step({"sender_multisig_forbidden_v"+std::to_string(v),"sender",q,multi({a->pubkey.get_addr()},1)});
        for(unsigned flags:{0,1,2,3,128,255})for(unsigned kind=0;kind<6;++kind){
            q=clone();q->inputs.resize(1);q->inputs[0].address=a->pubkey.get_addr();q->inputs[0].flags=flags;q->inputs[0].solution=0;
            if(kind==1)q->solutions={wrong};if(kind==2)q->inputs[0].solution=65535;if(kind==3)q->solutions={sol};
            std::shared_ptr<Contract> stored=multi({a->pubkey.get_addr()},1);if(kind==4)stored=nullptr;if(kind==5)stored=contract::Executable::create();
            emit_step({"input_f"+std::to_string(flags)+"_k"+std::to_string(kind)+"_v"+std::to_string(v),"input",q,stored});
        }
        for(unsigned deposit:{0,1})for(unsigned kind=0;kind<8;++kind){
            q=clone();auto op=deposit?std::shared_ptr<operation::Execute>(operation::Deposit::create()):operation::Execute::create();op->method="run";op->user=a->pubkey.get_addr();op->solution=0;
            if(kind==1)op->user=nullptr;if(kind==2)op->solution=65535;if(kind==3)q->solutions={sol};if(kind==4)q->solutions={wrong};
            q->execute={op};std::shared_ptr<Contract> stored=kind==3?multi({a->pubkey.get_addr()},1):nullptr;if(kind==5)stored=contract::Executable::create();
            Spec s{"user_d"+std::to_string(deposit)+"_k"+std::to_string(kind)+"_v"+std::to_string(v),"user",q,stored};if(kind==6)s.limit=1016;if(kind==7){s.cost=7;s.limit=1024;}emit_step(s);
        }
    }
    vnx::close();secp256k1_free();
}
