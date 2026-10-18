// Tiny native plot-format and farmer-derivation witnesses. Test seed only.
#include <mmx/ECDSA_Wallet.h>
#include <mmx/ProofOfSpaceOG.hxx>
#include <mmx/pos/Prover.h>
#include <mmx/pos/verify.h>
#include <vnx/vnx.h>
#include <fstream>

void bits(std::vector<uint8_t>& data, size_t offset, size_t count, uint64_t value) {
    for(size_t i = 0; i < count; ++i) data.at((offset+i)/8) |= ((value>>i)&1) << ((offset+i)%8);
}
int main(int argc, char** argv) {
    if(argc != 3) return 2;
    mmx::secp256k1_init();
    const auto proof = vnx::read_from_file<mmx::ProofOfSpaceOG>(argv[1]);
    if(!proof) throw std::runtime_error("expected OG fixture");
    std::vector<uint32_t> xs;
    const auto computed = mmx::pos::compute(proof->proof_xs, &xs, proof->plot_id, proof->ksize, 0);
    if(computed.size()!=1 || xs.size()!=256) throw std::runtime_error("expected one canonical proof");
    for(int mode=0; mode<3; ++mode) {
        const bool compressed = mode==2;
        auto h=mmx::PlotHeader::create();
        h->ksize=proof->ksize; h->xbits=h->ksize-int(compressed); h->has_meta=mode!=1;
        h->seed=proof->seed; h->plot_id=proof->plot_id; h->farmer_key=proof->farmer_key;
        h->park_size_x=128;h->park_size_y=1;h->park_size_pd=128;h->park_size_meta=1;
        h->park_bytes_x=256;h->park_bytes_y=4;h->park_bytes_pd=160;h->park_bytes_meta=12;
        h->entry_bits_x=16;h->num_entries_y=1;h->table_offset_y=4096;h->table_offset_meta=4100;h->table_offset_x=4112;
        for(int i=0;i<7;++i)h->table_offset_pd.push_back(4368+i*160);
        h->plot_size=5488;
        std::vector<uint8_t> data(h->plot_size);
        bits(data,4096*8,32,computed[0].first);
        uint32_t meta[12];memcpy(meta,computed[0].second.data(),48);
        for(size_t i=0;i<12;++i)bits(data,4100*8+i*8,8,meta[i]);
        for(size_t i=0;i<128;++i){
            uint32_t a=xs[2*i]>>int(compressed),b=xs[2*i+1]>>int(compressed);
            if(a<b)std::swap(a,b);
            if(compressed){++a;++b;if(a==b){++a;b=0;}}
            bits(data,4112*8+i*16,16,uint64_t(a)*(a-1)/2+b);
        }
        for(const auto base:h->table_offset_pd)for(size_t i=0;i<128;++i){bits(data,base*8+i*8,8,2*i);bits(data,base*8+1024+i*2,2,1);}
        const std::string path=std::string(argv[2])+"/native-"+std::to_string(mode)+".plot";
        vnx::write_to_file(path,h);
        std::fstream file(path,std::ios::in|std::ios::out|std::ios::binary);file.seekp(4096);file.write((const char*)data.data()+4096,data.size()-4096);file.close();
        mmx::pos::Prover prover(path);
        mmx::hash_t challenge;memcpy(challenge.data(),&computed[0].first,4);
        const auto qualities=prover.get_qualities(challenge,0);
        const auto full=prover.get_full_proof(0);
        if(qualities.size()!=1 || !qualities[0].valid || qualities[0].meta!=computed[0].second || !full.valid || full.proof!=xs)throw std::runtime_error("native plot round trip failed");
    }
    mmx::hash_t seed;for(size_t i=0;i<32;++i)seed[i]=i+1;
    mmx::ECDSA_Wallet wallet(seed,mmx::account_t(),mmx::ChainParams::create());
    std::ofstream out(std::string(argv[2])+"/farmer-key.txt");
    out<<wallet.get_farmer_key().first.to_string()<<"\n"<<wallet.get_farmer_key().second.to_string()<<"\n";
    mmx::secp256k1_free();
}
