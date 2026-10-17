#include <mmx/Block.hxx>
#include <mmx/ProofOfSpaceNFT.hxx>
#include <mmx/contract/Executable.hxx>
#include <mmx/contract/Binary.hxx>
#include <mmx/contract/Data.hxx>
#include <mmx/vm_interface.h>
#include <mmx/vm/Compiler.h>
#include <mmx/vm/StorageRAM.h>
#include <mmx/utils.h>
#include <bech32.h>
#include <vnx/vnx.h>
#include <fstream>
#include <iomanip>
#include <sstream>
namespace mmx {
struct NftReference {
    std::shared_ptr<ChainParams> params=ChainParams::create();
    std::shared_ptr<vm::StorageRAM> storage=std::make_shared<vm::StorageRAM>();
    std::map<addr_t,std::shared_ptr<Contract>> contracts;
    std::map<std::pair<addr_t,addr_t>,uint128> balances;
    mutable std::vector<addr_t> lookups;
    unsigned fail_on=0;
    uint32_t height=9;
    uint32_t get_height()const{return height;}
    template<class T>std::shared_ptr<const T> get_contract_as(const addr_t& a)const{lookups.push_back(a);if(fail_on&&lookups.size()==fail_on)throw std::logic_error("injected contract read failure");auto i=contracts.find(a);return i==contracts.end()?nullptr:std::dynamic_pointer_cast<const T>(i->second);}
    vnx::Variant call_contract(const addr_t&,const std::string&,const std::vector<vnx::Variant>&,const vnx::optional<addr_t>& user=nullptr,const vnx::optional<std::pair<addr_t,uint128>>& deposit=nullptr)const;
    addr_t get_plot_nft_target(const addr_t&,const vnx::optional<addr_t>&)const;
    void validate_reward(std::shared_ptr<const Block>)const;
};
// MMX_NFT_METHODS
}
using namespace mmx;using namespace mmx::vm;
std::string hex(const uint8_t* p,size_t n){std::ostringstream s;s<<std::hex<<std::setfill('0');for(size_t i=0;i<n;++i)s<<std::setw(2)<<unsigned(p[i]);return s.str();}
template<size_t N>std::string hx(const bytes_t<N>& p){return hex(p.data(),N);}
std::string raw(const var_t& v){const auto b=serialize(v,false,true);return hex(b.first.get(),b.second);}
std::string wire(std::shared_ptr<const vnx::Value> v){std::vector<uint8_t>b;vnx::VectorOutputStream s(&b);vnx::TypeOutput out(&s);vnx::write(out,v);out.flush();return hex(b.data(),b.size());}
template<class T>std::shared_ptr<T> decode(const std::string& text){const auto b=vnx::from_hex_string(text);vnx::VectorInputStream s(&b);vnx::TypeInput in(&s);std::shared_ptr<T> v;vnx::read(in,v);return v;}
addr_t addr(uint64_t n){addr_t a;a.from_uint(n);return a;}
skey_t secret(){skey_t k;k[31]=1;return k;}
template<class F>std::string outcome(F f){try{f();return "OK";}catch(const std::exception&e){return e.what();}}
std::ofstream output;size_t count=0;std::map<std::string,std::string> snapshots;
void emit(vnx::Object row,NftReference& node){
    vnx::Object state;state["params"]=wire(node.params);std::vector<vnx::Object> cs,vs;
    for(const auto&c:node.contracts){vnx::Object v;v["address"]=hx(c.first);v["wire"]=wire(c.second);cs.push_back(v);}state["contracts"]=cs;
    for(const auto&c:node.storage->get_memory()){vnx::Object v;v["contract"]=hx(c.first.first);v["address"]=c.first.second;v["value"]=raw(*c.second);vs.push_back(v);}state["cells"]=vs;
    for(const auto&c:node.storage->get_entries()){vnx::Object v;v["contract"]=hx(std::get<0>(c.first));v["address"]=std::get<1>(c.first);v["key"]=std::get<2>(c.first);v["value"]=raw(*c.second);vs.push_back(v);}state["cells"]=vs;
    std::vector<vnx::Object> bs;for(const auto&b:node.balances){vnx::Object v;v["contract"]=hx(b.first.first);v["currency"]=hx(b.first.second);v["amount"]=b.second.str();bs.push_back(v);}state["balances"]=bs;
    const auto key=vnx::to_string(state);auto old=snapshots.find(key);if(old==snapshots.end()){snapshots[key]=row["name"].to<std::string>();row["snapshot"]=state;}else row["seed"]=old->second;
    row["height"]=node.height;output<<vnx::to_string(row)<<'\n';++count;
}
auto bin(std::vector<instr_t> code,const std::vector<std::shared_ptr<var_t>>& values={},bool constant=true){
    auto b=contract::Binary::create();for(const auto&v:values){auto d=serialize(*v,false,false);b->constant.insert(b->constant.end(),d.first.get(),d.first.get()+d.second);}b->binary=serialize(code);
    contract::method_t m;m.name="mmx_reward_target";m.is_const=constant;m.is_public=true;b->methods[m.name]=m;return b;
}
NftReference base(std::shared_ptr<contract::Binary> b){NftReference n;n.params->hardfork2_height=10;n.params->max_tx_cost=1000000;auto e=contract::Executable::create();e->binary=addr(101);n.contracts[addr(1)]=e;n.contracts[addr(101)]=b;return n;}
void emit_target(std::string name,NftReference& n,vnx::optional<addr_t> farmer){
    vnx::Object row;row["kind"]="target";row["name"]=name;row["contract"]=hx(addr(1));row["farmer"]=farmer?hx(*farmer):std::string();addr_t target;n.lookups.clear();row["result"]=outcome([&]{target=n.get_plot_nft_target(addr(1),farmer);});row["target"]=hx(target);std::vector<std::string> lookups;for(const auto&a:n.lookups)lookups.push_back(hx(a));row["lookups"]=lookups;row["fail_on"]=n.fail_on;emit(row,n);
}
int main(int argc,char**argv){
    if(argc!=4)return 1;secp256k1_init();output.open(argv[1]);const uint32_t T=MEM_STACK,S=MEM_STATIC,E=MEM_EXTERN;
    auto original=std::make_shared<contract::Binary>(*vm::compile_files({std::string(argv[2])+"/src/contract/plot_nft.js"}));
    for(uint32_t height:{8,9,10,11,255,256,257})for(unsigned lock:{0,1,2})for(unsigned farmer:{0,1,2}){
        auto n=base(original);n.height=height;n.storage->write(addr(1),original->fields.at("owner"),*to_binary(addr(20)));n.storage->write(addr(1),original->fields.at("target"),*to_binary(addr(21)));
        if(lock==0)n.storage->write(addr(1),original->fields.at("unlock_height"),uint_t(0));if(lock==1)n.storage->write(addr(1),original->fields.at("unlock_height"),var_t());if(lock==2)n.storage->write(addr(1),original->fields.at("unlock_height"),uint_t(256));
        emit_target("plot_h"+std::to_string(height)+"_l"+std::to_string(lock)+"_f"+std::to_string(farmer),n,farmer?vnx::optional<addr_t>(addr(farmer==1?22:0)):nullptr);
    }
    std::vector<std::pair<std::string,std::shared_ptr<var_t>>> returns={
        {"nil",std::make_shared<var_t>()},{"true",std::make_shared<var_t>(TYPE_TRUE)},{"false",std::make_shared<var_t>(TYPE_FALSE)},
        {"small_uint",std::make_shared<uint_t>(0x1234)},{"max_u64",std::make_shared<uint_t>(UINT64_MAX)},{"wide_uint",std::make_shared<uint_t>(uint256_t(1)<<64)},
        {"valid_string",std::shared_ptr<var_t>(to_binary(addr(23).to_string()))},{"zero_string",std::shared_ptr<var_t>(to_binary(std::string("MMX")))},
        {"invalid_string",std::shared_ptr<var_t>(to_binary(std::string("invalid")))},{"empty_string",std::shared_ptr<var_t>(to_binary(std::string()))},
        {"non_utf8",std::shared_ptr<var_t>(to_binary(std::string(1,char(255))))},{"binary32",std::shared_ptr<var_t>(to_binary(addr(24)))}
    };
    for(unsigned len:{0,1,31,33,64}){std::vector<uint8_t> data(len);for(unsigned i=0;i<len;++i)data[i]=i+1;returns.emplace_back("binary"+std::to_string(len),std::shared_ptr<var_t>(binary_t::alloc(data.data(),data.size())));}
    for(const auto&r:returns)for(bool constant:{false,true})for(bool visible:{false,true}){auto b=bin({{OP_COPY,0,T,0},{OP_RET}},{r.second},constant);b->methods["mmx_reward_target"].is_public=visible;b->methods["mmx_reward_target"].is_init=!visible;auto n=base(b);emit_target(r.first+"_c"+std::to_string(constant)+"_p"+std::to_string(visible),n,addr(22));}
    auto typed=[&](std::string name,std::vector<instr_t> code,const std::vector<std::shared_ptr<var_t>>& values={},bool constant=true){auto n=base(bin(code,values,constant));emit_target(name,n,addr(22));};
    typed("return_argument",{{OP_COPY,0,T,T+1},{OP_RET}});typed("return_missing",{{OP_RET}});
    typed("return_address",{{OP_COPY,0,T,E+EXTERN_ADDRESS},{OP_RET}});typed("return_txid",{{OP_COPY,0,T,E+EXTERN_TXID},{OP_RET}});typed("return_user",{{OP_COPY,0,T,E+EXTERN_USER},{OP_RET}});
    typed("return_network",{{OP_COPY,0,T,E+EXTERN_NETWORK},{OP_RET}});typed("return_height",{{OP_COPY,0,T,E+EXTERN_HEIGHT},{OP_RET}});
    typed("remote_unavailable",{{OP_RCALL,0,0,0,0,0},{OP_RET}});typed("cread_unavailable",{{OP_CREAD,0,T,0,0},{OP_RET}});
    typed("write_new_static",{{OP_COPY,0,S,0},{OP_COPY,0,T,0},{OP_RET}},{std::shared_ptr<var_t>(to_binary(addr(25).to_string()))},false);
    for(bool constant:{false,true}){auto n=base(bin({{OP_COPY,0,S,0},{OP_COPY,0,T,0},{OP_RET}},{std::shared_ptr<var_t>(to_binary(addr(25).to_string()))},constant));n.storage->write(addr(1),S,uint_t(7));emit_target("write_stored_c"+std::to_string(constant),n,addr(22));}
    for(unsigned length:{0,1,31,32,33,64}){
        auto b=std::make_shared<contract::Binary>(*vm::compile("function mmx_reward_target(farmer) const public { var out = []; for(var i = 0; i < "+std::to_string(length)+"; i++) { push(out, i + 257); } return out; }"));auto n=base(b);emit_target("array"+std::to_string(length),n,addr(22));
    }
    for(auto src:std::vector<std::string>{"return {};","return [true, false, null, 257, \"invalid\", [1], {}];","return {a: [1, 2]};","fail(\"target failed\", 17);"}){auto n=base(std::make_shared<contract::Binary>(*vm::compile("function mmx_reward_target(farmer) const public { "+src+" }")));emit_target("compiled"+std::to_string(count),n,addr(22));}
    for(unsigned kind=0;kind<6;++kind){auto n=base(bin({{OP_RET}}));if(kind==0)n.contracts.clear();if(kind==1)n.contracts[addr(1)]=contract::Data::create();if(kind==2)n.contracts.erase(addr(101));if(kind==3)n.contracts[addr(101)]=contract::Data::create();if(kind==4)std::dynamic_pointer_cast<contract::Binary>(n.contracts[addr(101)])->methods.clear();if(kind==5)std::dynamic_pointer_cast<contract::Binary>(n.contracts[addr(101)])->binary={255};emit_target("lookup"+std::to_string(kind),n,nullptr);}
    auto gas=base(bin({{OP_COPY,0,T,T+1},{OP_RET}}));for(uint64_t limit:{0,1,19,20,49,50,69,70,100,1000}){gas.params->max_tx_cost=limit;emit_target("gas"+std::to_string(limit),gas,addr(22));}
    auto stored=base(bin({{OP_COPY,0,T,S},{OP_RET}}));map_t m;m.address=S;stored.storage->write(addr(1),S,m);emit_target("stored_map",stored,addr(22));
    {auto n=base(bin({{OP_COPY,0,T,0},{OP_RET}},{std::make_shared<ref_t>(S)}));map_t map;map.address=S;n.storage->write(addr(1),S,map);emit_target("stored_map_ref",n,addr(22));}
    for(bool bad:{false,true}){auto n=base(bin({{OP_COPY,0,T,0},{OP_RET}},{std::make_shared<ref_t>(S)}));array_t arr(34);arr.address=S;n.storage->write(addr(1),S,arr);for(unsigned i=0;i<34;++i)n.storage->write(addr(1),S,i,uint_t(257+i));if(bad){map_t map;map.address=S+100;n.storage->write(addr(1),S+100,map);n.storage->write(addr(1),S,33,ref_t(S+100));}emit_target(bad?"array_tail_stored_map":"stored_array34",n,addr(22));}
    {auto n=base(bin({{OP_SEND,0,1,2,3,0},{OP_MINT,0,1,2,0},{OP_COPY,0,T,4},{OP_RET}},
        {std::make_shared<var_t>(),std::shared_ptr<var_t>(to_binary(addr(26))),std::make_shared<uint_t>(7),std::shared_ptr<var_t>(to_binary(addr_t())),std::shared_ptr<var_t>(to_binary(addr(25).to_string()))},false));n.balances[{addr(1),addr(0)}]=20;n.storage->set_balance(addr(1),addr(0),20);emit_target("discard_transfers",n,addr(22));}
    {auto n=base(std::make_shared<contract::Binary>(*vm::compile("function mmx_reward_target(farmer) const public { assert(this.txid == null); assert(this.user == null); return farmer; }")));emit_target("nil_externals",n,addr(22));}
    for(uint64_t limit:{545,546,565,566}){gas.params->max_tx_cost=limit;emit_target("gas_boundary"+std::to_string(limit),gas,addr(22));}
    {auto b=bin({{OP_RET}});b->methods.clear();b->binary={255};auto n=base(b);emit_target("no_method_bad_bytecode",n,nullptr);}
    {const std::string chars="023456789acdef",alphabet="qpzry9x8gf2tvdw0s3jn54khce6mua7l";bool found=false;for(unsigned trial=0;trial<100000;++trial){std::vector<uint8_t> words(52,alphabet.find('0'));unsigned v=trial;for(unsigned i=0;i<8;++i){words[i]=alphabet.find(chars[v%chars.size()]);v/=chars.size();}const auto text=bech32::encode("0x",words);if(text.substr(2).find_first_not_of("0123456789abcdef")!=std::string::npos)continue;auto n=base(bin({{OP_COPY,0,T,0},{OP_RET}},{std::make_shared<uint_t>(uint256_t(text.substr(2),16))}));emit_target("hex_uint_valid_address",n,nullptr);found=true;break;}if(!found)throw std::logic_error("could not find hex address witness");}
    for(unsigned fail=1;fail<=4;++fail){auto n=base(bin({{OP_COPY,0,T,T+1},{OP_RET}}));n.fail_on=fail;emit_target("lookup_failure"+std::to_string(fail),n,addr(22));}
    std::ifstream work(argv[3]);std::string line;
    while(std::getline(work,line)){
        auto seed=vnx::from_string<vnx::Object>(line);if(seed["kind"].to<std::string>()!="pipeline"||seed["result"].to<std::string>()!="OK")continue;
        for(unsigned mode=0;mode<9;++mode){auto b=decode<Block>(seed["block"].to<std::string>());auto parent=decode<BlockHeader>(seed["parent"].to<std::string>());auto n=base(original);n.params=decode<ChainParams>(seed["params"].to<std::string>());n.height=parent->height;
            n.storage->write(addr(1),original->fields.at("owner"),*to_binary(addr(20)));n.storage->write(addr(1),original->fields.at("target"),*to_binary(addr(21)));n.storage->write(addr(1),original->fields.at("unlock_height"),uint_t(parent->height+1));
            auto nft=ProofOfSpaceNFT::create();nft->ksize=8;nft->farmer_key=b->proof[0]->farmer_key;nft->contract=addr(1);b->proof={nft};b->reward_addr=addr(21);b->reward_contract=addr(1);b->reward_account=addr(22);
            if(mode==1)b->reward_addr=addr(22);if(mode==2)b->reward_contract=nullptr;if(mode==3)b->reward_contract=addr(2);if(mode==4){b->reward_addr=nullptr;b->reward_contract=nullptr;}if(mode==5){b->reward_addr=nullptr;}if(mode==6){n.contracts.clear();b->reward_addr=addr(1);}if(mode==7){b->proof=decode<Block>(seed["block"].to<std::string>())->proof;b->reward_contract=nullptr;}if(mode==8){b->proof=decode<Block>(seed["block"].to<std::string>())->proof;}
            b->hash=b->calc_hash(n.params);b->farmer_sig=signature_t::sign(secret(),b->hash);b->content_hash=b->calc_content_hash();
            vnx::Object row;row["kind"]="block";row["name"]="block_h"+std::to_string(b->height)+"_m"+std::to_string(mode);row["block"]=wire(b);row["parent"]=wire(parent);row["result"]=outcome([&]{if(!b->is_valid(n.params))throw std::logic_error("static validation failed");b->validate();n.validate_reward(b);});emit(row,n);
        }
    }
    std::cerr<<"wrote "<<count<<" native NFT cases\n";
}
