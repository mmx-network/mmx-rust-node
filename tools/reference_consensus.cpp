// Native hashing / static validation oracle. No node or chain data required.
#include <mmx/Transaction.hxx>
#include <mmx/contract/Binary.hxx>
#include <mmx/contract/Executable.hxx>
#include <mmx/contract/Data.hxx>
#include <mmx/contract/WebData.hxx>
#include <mmx/contract/PubKey.hxx>
#include <mmx/contract/MultiSig.hxx>
#include <mmx/operation/Execute.hxx>
#include <mmx/operation/Deposit.hxx>
#include <mmx/solution/PubKey.hxx>
#include <mmx/solution/MultiSig.hxx>
#include <mmx/write_bytes.h>
#include <vnx/vnx.h>
#include <fstream>
#include <iomanip>
#include <sstream>
#include <limits>
using namespace mmx;
std::ofstream output;
std::string hex(const uint8_t* data,size_t size) { std::ostringstream s;s<<std::hex<<std::setfill('0');for(size_t i=0;i<size;++i)s<<std::setw(2)<<unsigned(data[i]);return s.str(); }
template<size_t N> std::string hx(const bytes_t<N>& b){return hex(b.data(),N);}
template<class F> std::string attempt(F f){try{return f();}catch(const std::exception& e){return "ERR";}}
addr_t addr(uint64_t n){addr_t a;a.from_uint(n);return a;}
auto params(unsigned profile=0){auto p=ChainParams::create();p->network="mainnet";if(profile==1){p->min_txfee=17;p->min_txfee_io=19;p->min_txfee_memo=23;p->min_txfee_byte=29;p->min_txfee_exec=31;p->min_txfee_sign=37;p->min_txfee_deploy=41;p->min_txfee_depend=43;}if(profile==2){p->min_txfee=UINT64_MAX;p->min_txfee_io=UINT64_MAX;p->min_txfee_memo=UINT64_MAX;p->min_txfee_byte=UINT64_MAX;p->min_txfee_exec=UINT64_MAX;p->min_txfee_sign=UINT64_MAX;p->min_txfee_deploy=UINT64_MAX;p->min_txfee_depend=UINT64_MAX;}return p;}
template<class T> void emit(const std::string& name,const T& value,unsigned profile=0){
    auto p=params(profile);std::vector<uint8_t> bytes;vnx::VectorOutputStream stream(&bytes);vnx::TypeOutput out(&stream);vnx::write(out,value,nullptr,nullptr);out.flush();
    output<<name<<'\t'<<profile<<'\t'<<hex(bytes.data(),bytes.size());
    output<<'\t'<<attempt([&]{if constexpr(std::is_base_of<vnx::Value,T>::value)return std::string(value.is_valid(p)?"1":"0");else return std::string(value.is_valid()?"1":"0");});
    output<<'\t'<<attempt([&]{return std::to_string(value.calc_cost(p));});
    output<<'\t'<<attempt([&]{if constexpr(std::is_base_of<Contract,T>::value)return std::to_string(value.num_bytes());else return std::string("-");});
    for(unsigned version:{0,1})for(bool full:{false,true})output<<'\t'<<attempt([&]{if constexpr(std::is_base_of<Contract,T>::value||std::is_base_of<Operation,T>::value)return hx(value.calc_hash(full,version));else if constexpr(std::is_base_of<TransactionBase,T>::value)return hx(value.calc_hash(full));else return hx(value.calc_hash());});
    if constexpr(std::is_same<Transaction,T>::value){for(bool full:{false,true}){const auto b=value.hash_serialize(full);output<<'\t'<<hex(b.data(),b.size());}}else output<<"\t-\t-";
    output<<'\n';
}
// Class is_valid signatures differ from Transaction::is_valid(params).
template<class T> void emit_class(const std::string& name,const std::shared_ptr<T>& value,unsigned profile=0){
    auto p=params(profile);std::vector<uint8_t> bytes;vnx::VectorOutputStream stream(&bytes);vnx::TypeOutput out(&stream);vnx::write(out,value);out.flush();
    output<<name<<'\t'<<profile<<'\t'<<hex(bytes.data(),bytes.size())<<'\t'<<attempt([&]{return std::string(value->is_valid()?"1":"0");})<<'\t'<<attempt([&]{return std::to_string(value->calc_cost(p));})<<'\t';
    if constexpr(std::is_base_of<Contract,T>::value)output<<attempt([&]{return std::to_string(value->num_bytes());});else output<<'-';
    for(unsigned version:{0,1})for(bool full:{false,true})output<<'\t'<<attempt([&]{if constexpr(std::is_base_of<Solution,T>::value)return hx(value->calc_hash());else return hx(value->calc_hash(full,version));});
    output<<"\t-\t-\n";
}
void seal(std::shared_ptr<Transaction> tx){tx->nonce=1;tx->id=tx->calc_hash();tx->static_cost=tx->calc_cost(params());tx->content_hash=tx->calc_hash(true);}
auto transaction(unsigned version){auto tx=Transaction::create();tx->version=version;tx->network="mainnet";seal(tx);return tx;}
void et(const std::string& name,std::shared_ptr<Transaction> tx){emit(name,*tx);}
int main(int argc,char**argv){
    vnx::init("reference_consensus",argc,argv);output.open(argv[1]);
    // Scalar widths, signs, raw strings, nested JSON, and non-JSON variants.
    vnx::Object object;object["z"]=uint64_t(-1);object["a"]=int64_t(1);object[std::string(1,char(255))]=std::string({char(255),char(0)});
    std::vector<vnx::Variant> values={vnx::Variant(),vnx::Variant(false),vnx::Variant(true),vnx::Variant(uint8_t(1)),vnx::Variant(uint16_t(65535)),vnx::Variant(uint32_t(-1)),vnx::Variant(uint64_t(-1)),vnx::Variant(int8_t(-1)),vnx::Variant(int16_t(-32768)),vnx::Variant(int32_t(1)),vnx::Variant(int64_t(-1)),vnx::Variant(int64_t(1)),vnx::Variant(std::string("abc")),vnx::Variant(std::string({char(255),char(0)})),vnx::Variant(object),vnx::Variant(std::vector<vnx::Variant>{vnx::Variant(object),vnx::Variant(uint64_t(7))}),vnx::Variant(std::vector<uint8_t>{0,1,255}),vnx::Variant(std::vector<int8_t>{-1,0,1}),vnx::Variant(std::vector<uint32_t>{0,1,65536}),vnx::Variant(1.25),vnx::Variant(std::make_pair(int32_t(1),std::string("two"))),vnx::Variant(std::numeric_limits<double>::quiet_NaN())};
    for(const auto& raw:std::vector<std::vector<uint8_t>>{
        {1,0,0,4,0,0,0,0,0,0,0,7}, // alternate uint64
        {2,0,12,0,0,5,2,0,0,0,255,1}, // little list + alternate int8: array
        {2,0,0,12,5,0,0,0,0,2,255,1}, // alternate list + little int8: array
        {2,0,0,12,0,5,0,0,0,2,255,1}, // alternate character list: string
        {0,1,0,4,0,0,0,0,0,0,0,7}, // alternate dynamic length header
        {1,0,0,24,0,0,0,1,0,0,0,1,97,1,0,1,0,7}, // alternate object, little dynamic header
    }){vnx::Variant v;v.data=raw;values.push_back(v);}
    values.push_back(vnx::Variant(std::array<int16_t,3>{-1,0,1}));
    values.push_back(vnx::Variant(std::vector<float>{1.5f,-2.0f}));
    values.push_back(vnx::Variant(std::vector<double>{1.5,-2.0}));
    for(const auto& first:std::vector<vnx::Variant>{vnx::Variant(uint64_t(1)),vnx::Variant(1.5)}){
        vnx::Variant v;v.data={1,0,24,0,2,0,0,0,1,0,0,0,97};
        v.data.insert(v.data.end(),first.data.begin(),first.data.end());
        const std::vector<uint8_t> tail={1,0,0,0,97,1,0,1,0,7};v.data.insert(v.data.end(),tail.begin(),tail.end());values.push_back(v);
    }
    for(size_t i=0;i<values.size();++i){
        auto d=contract::Data::create();d->value=values[i];emit_class("data_"+std::to_string(i),d);if(i%4==0)emit_class("data_custom_"+std::to_string(i),d,1);
        auto e=contract::Executable::create();e->name="Token";e->symbol="T";e->decimals=18;e->meta_data=values[i];e->binary=addr(4);e->init_args={values[i]};e->depends["z"]=addr(257);e->depends[std::string(1,char(255))]=addr(2);emit_class("executable_"+std::to_string(i),e);
        auto op=operation::Deposit::create();op->address=addr(1);op->currency=addr(2);op->amount=uint128(1)<<127;op->method=std::string({char(255),char(0)});op->args={values[i]};op->user=addr(3);op->solution=7;emit_class("deposit_"+std::to_string(i),op);
        for(unsigned version:{0,1}){auto tx=transaction(version);tx->execute={op};tx->deploy=e;tx->inputs.resize(1);tx->inputs[0].amount=(uint128(1)<<100)+123;tx->inputs[0].solution=4;tx->inputs[0].flags=0x91;tx->inputs[0].memo=std::string({char(255),char(0)});tx->outputs.resize(1);tx->outputs[0].amount=uint128(-1);seal(tx);et("tx_variant_"+std::to_string(i)+"_v"+std::to_string(version),tx);}
    }
    for(unsigned depth:{48,49,50,51,52}){vnx::Variant v(1);for(unsigned i=0;i<depth;++i){std::vector<uint8_t> raw={2,0,12,0,17,0,1,0,0,0};raw.insert(raw.end(),v.data.begin(),v.data.end());v.data=raw;}auto d=contract::Data::create();d->value=v;emit_class("recursion_"+std::to_string(depth),d);}
    auto pub=contract::PubKey::create();emit_class("pubkey_zero",pub);pub->address=addr(9);emit_class("pubkey",pub);pub->version=1;emit_class("pubkey_version",pub);
    for(unsigned required:{0,1,2,3}){auto m=contract::MultiSig::create();m->owners={addr(1),addr(256)};m->num_required=required;emit_class("multisig_"+std::to_string(required),m);}
    for(unsigned n:{0,1,256,257}){auto w=contract::WebData::create();w->mime_type=std::string(n,'x');w->payload=std::string({char(0),char(255)});emit_class("web_"+std::to_string(n),w);}
    for(unsigned n:{0,6,7})for(int dec:{-1,0,18,19}){auto t=contract::TokenBase::create();t->name=std::string(64,'N');t->symbol=std::string(n,'T');t->decimals=dec;emit_class("token_"+std::to_string(n)+"_"+std::to_string(dec),t);}
    for(const auto& symbol:std::vector<std::string>{"MMX","mmx","A B","A\tB",std::string(1,char(255))}){auto t=contract::TokenBase::create();t->symbol=symbol;emit_class("symbol_"+hex((const uint8_t*)symbol.data(),symbol.size()),t);}
    for(unsigned flags=0;flags<16;++flags){auto b=contract::Binary::create();b->name=std::string({char(255),char(0)});b->fields["a"]=7;b->fields["z"]=UINT32_MAX;contract::method_t m;m.name="name";m.info="info";m.is_init=flags&1;m.is_const=flags&2;m.is_public=flags&4;m.is_payable=flags&8;m.entry_point=123;m.args={"a","b"};b->methods["method"]=m;b->constant={0,1,255};b->binary={255,0,1};b->line_info[2]=9;b->line_info[999]=UINT32_MAX;b->source="source";b->compiler="compiler";b->build_flags.verbose=-1;b->build_flags.opt_level=3;b->build_flags.catch_overflow=true;emit_class("binary_"+std::to_string(flags),b);if(flags==0)emit_class("binary_custom",b,1);}
    auto pk=solution::PubKey::create();emit_class("solution_pubkey",pk);pk->pubkey.data()[0]=2;pk->signature.data()[0]=255;emit_class("solution_pubkey_data",pk);pk->version=1;emit_class("solution_pubkey_version",pk);pk->version=0;
    auto ms=solution::MultiSig::create();ms->solutions[addr(256)]=pk;ms->solutions[addr(1)]=nullptr;ms->num_required=7;emit_class("solution_multi_null",ms);ms->solutions[addr(1)]=solution::MultiSig::create();emit_class("solution_multi_nested",ms);emit_class("solution_multi_overflow",ms,2);
    for(unsigned n:{0,1,31,32,33,64,65}){
        exec_result_t r;r.did_fail=true;r.total_fee=7;r.total_cost=11;r.inputs.resize(1);r.inputs[0].solution=8;r.inputs[0].flags=255;r.inputs[0].memo=std::string(n,'i');r.outputs.resize(1);r.outputs[0].memo=std::string(n,'o');exec_error_t err;err.code=9;err.address=17;err.operation=2;err.line=23;err.message="message";r.error=err;emit("result_memo_"+std::to_string(n),r);auto tx=transaction(1);tx->exec_result=r;seal(tx);et("tx_result_"+std::to_string(n),tx);
    }
    for(unsigned n:{160,161}){exec_result_t r;exec_error_t err;err.message=std::string(n,'x');r.error=err;emit("error_message_"+std::to_string(n),r);}
    for(unsigned version:{0,1,2}){
        auto base=transaction(version);et("tx_base_v"+std::to_string(version),base);
        auto change=[&](std::string name,auto edit,bool reseal=true){auto tx=std::make_shared<Transaction>(*base);edit(*tx);if(reseal)seal(tx);et(name+"_v"+std::to_string(version),tx);};
        change("nonce_zero",[](auto&tx){tx.nonce=0;},false);
        change("ratio_1023",[](auto&tx){tx.fee_ratio=1023;});change("ratio_1024",[](auto&tx){tx.fee_ratio=1024;});change("wrong_network",[](auto&tx){tx.network="testnet";});
        change("wrong_id",[](auto&tx){tx.id=hash_t();},false);change("wrong_content",[](auto&tx){tx.content_hash=hash_t();},false);change("wrong_cost",[](auto&tx){++tx.static_cost;},false);
        change("null_operation",[](auto&tx){tx.execute={nullptr};});change("null_solution",[](auto&tx){tx.solutions={nullptr};});
        change("unused_solution",[&](auto&tx){tx.solutions={pk};});change("sender_solution",[&](auto&tx){tx.sender=addr(1);tx.solutions={pk};});
        change("duplicate_solutions",[&](auto&tx){tx.sender=addr(1);tx.solutions={pk,pk};tx.inputs.resize(1);tx.inputs[0].solution=1;});
        change("input_solution",[&](auto&tx){tx.solutions={pk};tx.inputs.resize(1);tx.inputs[0].solution=0;});
        change("out_of_range_solution",[&](auto&tx){tx.solutions={pk};tx.inputs.resize(1);tx.inputs[0].solution=1;});
        change("user_solution",[&](auto&tx){tx.solutions={pk};auto op=operation::Execute::create();op->method="run";op->user=addr(1);op->solution=0;tx.execute={op};});
        change("empty_method",[&](auto&tx){tx.execute={operation::Execute::create()};});
        change("zero_deposit",[&](auto&tx){auto op=operation::Deposit::create();op->method="run";tx.execute={op};});
        change("invalid_deploy_still_statically_valid",[&](auto&tx){auto d=contract::PubKey::create();d->version=1;tx.deploy=d;});
        for(unsigned n:{0,32,33,64,65})change("input_memo_"+std::to_string(n),[&](auto&tx){tx.inputs.resize(1);tx.inputs[0].memo=std::string(n,'m');});
        emit("tx_cost_overflow_v"+std::to_string(version),*base,2);
        auto result=std::make_shared<Transaction>(*base);result->inputs.resize(1);result->outputs.resize(1);result->sender=addr(1);result->solutions={pk};seal(result);emit("tx_io_cost_overflow_v"+std::to_string(version),*result,2);
    }
    // Deterministic combinations exercise every transaction header and txio
    // field, arbitrary 128-bit amounts, solution indices/flags, and both hashes.
    uint64_t rng=0x76e3c29b4185adf0ull;
    auto random=[&](){rng^=rng<<13;rng^=rng>>7;rng^=rng<<17;return rng;};
    for(unsigned i=0;i<500;++i){
        auto tx=transaction(i%2);tx->expires=random();tx->fee_ratio=1024+(random()%65536);tx->max_fee_amount=random();tx->note=tx_note_e::enum_t(uint32_t(random()));tx->nonce=random();
        if(i%3)tx->sender=addr(random());
        for(unsigned j=0,n=random()%6;j<n;++j){txin_t in;in.address=addr(random());in.contract=addr(random());in.amount=(uint128(random())<<64)+random();in.solution=random();in.flags=random();if(j%2)in.memo=std::string(random()%67,char(random()));tx->inputs.push_back(in);}
        for(unsigned j=0,n=random()%6;j<n;++j){txout_t out;out.address=addr(random());out.contract=addr(random());out.amount=(uint128(random())<<64)+random();if(j%2)out.memo=std::string(random()%67,char(random()));tx->outputs.push_back(out);}
        for(unsigned j=0,n=random()%4;j<n;++j){auto op=operation::Execute::create();op->address=addr(random());op->method=std::string(1+random()%16,char(random()));op->args={values[random()%values.size()]};op->solution=random();if(j%2)op->user=addr(random());tx->execute.push_back(op);}
        for(unsigned j=0,n=random()%5;j<n;++j){auto sol=solution::PubKey::create();for(auto& b:sol->pubkey.bytes)b=random();for(auto& b:sol->signature.bytes)b=random();tx->solutions.push_back(sol);}
        tx->id=tx->calc_hash();tx->static_cost=tx->calc_cost(params());tx->content_hash=tx->calc_hash(true);et("combined_"+std::to_string(i),tx);
    }
    vnx::close();
}
