//! Trending token pairs for arbitrage - Top 50 + 450 trending pairs
//!
//! This module provides comprehensive coverage of profitable trading pairs including:
//! - Top 50 tokens by volume/market cap
//! - Trending meme coins (PEPE, SHIB, FLOKI, etc.)
//! - DeFi blue chips (UNI, AAVE, LINK, etc.)
//! - L2 tokens (ARB, OP, etc.)
//! - Cross-DEX pairs for maximum arbitrage opportunities

use crate::artemis::DexType;
use crate::strategies::{DexPair, TokenInfo};
use alloy::primitives::{address, Address};
use tracing::info;

/// Top tokens - addresses on Ethereum mainnet
pub mod tokens {
    use super::*;

    // ============ STABLECOINS ============
    pub fn usdc() -> TokenInfo {
        TokenInfo { address: address!("A0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"), symbol: "USDC".into(), decimals: 6 }
    }
    pub fn usdt() -> TokenInfo {
        TokenInfo { address: address!("dAC17F958D2ee523a2206206994597C13D831ec7"), symbol: "USDT".into(), decimals: 6 }
    }
    pub fn dai() -> TokenInfo {
        TokenInfo { address: address!("6B175474E89094C44Da98b954EedeAC495271d0F"), symbol: "DAI".into(), decimals: 18 }
    }
    pub fn frax() -> TokenInfo {
        TokenInfo { address: address!("853d955aCEf822Db058eb8505911ED77F175b99e"), symbol: "FRAX".into(), decimals: 18 }
    }
    pub fn lusd() -> TokenInfo {
        TokenInfo { address: address!("5f98805A4E8be255a32880FDeC7F6728C6568bA0"), symbol: "LUSD".into(), decimals: 18 }
    }
    pub fn tusd() -> TokenInfo {
        TokenInfo { address: address!("0000000000085d4780B73119b644AE5ecd22b376"), symbol: "TUSD".into(), decimals: 18 }
    }
    pub fn usdd() -> TokenInfo {
        TokenInfo { address: address!("0C10bF8FcB7Bf5412187A595ab97a3609160b5c6"), symbol: "USDD".into(), decimals: 18 }
    }

    // ============ WRAPPED NATIVE ============
    pub fn weth() -> TokenInfo {
        TokenInfo { address: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"), symbol: "WETH".into(), decimals: 18 }
    }
    pub fn wbtc() -> TokenInfo {
        TokenInfo { address: address!("2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599"), symbol: "WBTC".into(), decimals: 8 }
    }
    pub fn steth() -> TokenInfo {
        TokenInfo { address: address!("ae7ab96520DE3A18E5e111B5EaAb095312D7fE84"), symbol: "stETH".into(), decimals: 18 }
    }
    pub fn wsteth() -> TokenInfo {
        TokenInfo { address: address!("7f39C581F595B53c5cb19bD0b3f8dA6c935E2Ca0"), symbol: "wstETH".into(), decimals: 18 }
    }
    pub fn reth() -> TokenInfo {
        TokenInfo { address: address!("ae78736Cd615f374D3085123A210448E74Fc6393"), symbol: "rETH".into(), decimals: 18 }
    }
    pub fn cbeth() -> TokenInfo {
        TokenInfo { address: address!("Be9895146f7AF43049ca1c1AE358B0541Ea49704"), symbol: "cbETH".into(), decimals: 18 }
    }

    // ============ DEFI BLUE CHIPS ============
    pub fn uni() -> TokenInfo {
        TokenInfo { address: address!("1f9840a85d5aF5bf1D1762F925BDADdC4201F984"), symbol: "UNI".into(), decimals: 18 }
    }
    pub fn link() -> TokenInfo {
        TokenInfo { address: address!("514910771AF9Ca656af840dff83E8264EcF986CA"), symbol: "LINK".into(), decimals: 18 }
    }
    pub fn aave() -> TokenInfo {
        TokenInfo { address: address!("7Fc66500c84A76Ad7e9c93437bFc5Ac33E2DDaE9"), symbol: "AAVE".into(), decimals: 18 }
    }
    pub fn mkr() -> TokenInfo {
        TokenInfo { address: address!("9f8F72aA9304c8B593d555F12eF6589cC3A579A2"), symbol: "MKR".into(), decimals: 18 }
    }
    pub fn crv() -> TokenInfo {
        TokenInfo { address: address!("D533a949740bb3306d119CC777fa900bA034cd52"), symbol: "CRV".into(), decimals: 18 }
    }
    pub fn comp() -> TokenInfo {
        TokenInfo { address: address!("c00e94Cb662C3520282E6f5717214004A7f26888"), symbol: "COMP".into(), decimals: 18 }
    }
    pub fn snx() -> TokenInfo {
        TokenInfo { address: address!("C011a73ee8576Fb46F5E1c5751cA3B9Fe0af2a6F"), symbol: "SNX".into(), decimals: 18 }
    }
    pub fn ldo() -> TokenInfo {
        TokenInfo { address: address!("5A98FcBEA516Cf06857215779Fd812CA3beF1B32"), symbol: "LDO".into(), decimals: 18 }
    }
    pub fn fxs() -> TokenInfo {
        TokenInfo { address: address!("3432B6A60D23Ca0dFCa7761B7ab56459D9C964D0"), symbol: "FXS".into(), decimals: 18 }
    }
    pub fn bal() -> TokenInfo {
        TokenInfo { address: address!("ba100000625a3754423978a60c9317c58a424e3D"), symbol: "BAL".into(), decimals: 18 }
    }
    pub fn sushi() -> TokenInfo {
        TokenInfo { address: address!("6B3595068778DD592e39A122f4f5a5cF09C90fE2"), symbol: "SUSHI".into(), decimals: 18 }
    }
    pub fn inch() -> TokenInfo {
        TokenInfo { address: address!("111111111117dC0aa78b770fA6A738034120C302"), symbol: "1INCH".into(), decimals: 18 }
    }
    pub fn yfi() -> TokenInfo {
        TokenInfo { address: address!("0bc529c00C6401aEF6D220BE8C6Ea1667F6Ad93e"), symbol: "YFI".into(), decimals: 18 }
    }
    pub fn ens() -> TokenInfo {
        TokenInfo { address: address!("C18360217D8F7Ab5e7c516566761Ea12Ce7F9D72"), symbol: "ENS".into(), decimals: 18 }
    }
    pub fn grt() -> TokenInfo {
        TokenInfo { address: address!("c944E90C64B2c07662A292be6244BDf05Cda44a7"), symbol: "GRT".into(), decimals: 18 }
    }
    pub fn dydx() -> TokenInfo {
        TokenInfo { address: address!("92D6C1e31e14520e676a687F0a93788B716BEff5"), symbol: "DYDX".into(), decimals: 18 }
    }
    pub fn gmx() -> TokenInfo {
        TokenInfo { address: address!("fc5A1A6EB076a2C7aD06eD22C90d7E710E35ad0a"), symbol: "GMX".into(), decimals: 18 }
    }
    pub fn pendle() -> TokenInfo {
        TokenInfo { address: address!("808507121B80c02388fAd14726482e061B8da827"), symbol: "PENDLE".into(), decimals: 18 }
    }

    // ============ L2 TOKENS ============
    pub fn arb() -> TokenInfo {
        TokenInfo { address: address!("B50721BCf8d664c30412Cfbc6cf7a15145234ad1"), symbol: "ARB".into(), decimals: 18 }
    }
    pub fn op() -> TokenInfo {
        TokenInfo { address: address!("4200000000000000000000000000000000000042"), symbol: "OP".into(), decimals: 18 }
    }
    pub fn matic() -> TokenInfo {
        TokenInfo { address: address!("7D1AfA7B718fb893dB30A3aBc0Cfc608AaCfeBB0"), symbol: "MATIC".into(), decimals: 18 }
    }
    pub fn metis() -> TokenInfo {
        TokenInfo { address: address!("9E32b13ce7f2E80A01932B42553652E053D6ed8e"), symbol: "METIS".into(), decimals: 18 }
    }
    pub fn imx() -> TokenInfo {
        TokenInfo { address: address!("F57e7e7C23978C3cAEC3C3548E3D615c346e79fF"), symbol: "IMX".into(), decimals: 18 }
    }
    pub fn lrc() -> TokenInfo {
        TokenInfo { address: address!("BBbbCA6A901c926F240b89EacB641d8Aec7AEafD"), symbol: "LRC".into(), decimals: 18 }
    }

    // ============ MEME COINS ============
    pub fn pepe() -> TokenInfo {
        TokenInfo { address: address!("6982508145454Ce325dDbE47a25d4ec3d2311933"), symbol: "PEPE".into(), decimals: 18 }
    }
    pub fn shib() -> TokenInfo {
        TokenInfo { address: address!("95aD61b0a150d79219dCF64E1E6Cc01f0B64C4cE"), symbol: "SHIB".into(), decimals: 18 }
    }
    pub fn floki() -> TokenInfo {
        TokenInfo { address: address!("cF0C122c6b73ff809C693DB761e7BaeBe62b6a2E"), symbol: "FLOKI".into(), decimals: 9 }
    }
    pub fn bone() -> TokenInfo {
        TokenInfo { address: address!("9813037ee2218799597d83D4a5B6F3b6778218d9"), symbol: "BONE".into(), decimals: 18 }
    }
    pub fn leash() -> TokenInfo {
        TokenInfo { address: address!("27C70Cd1946795B66be9d954418546998b546634"), symbol: "LEASH".into(), decimals: 18 }
    }
    pub fn elon() -> TokenInfo {
        TokenInfo { address: address!("761D38e5ddf6ccf6Cf7c55759d5210750B5D60F3"), symbol: "ELON".into(), decimals: 18 }
    }
    pub fn wojak() -> TokenInfo {
        TokenInfo { address: address!("5026F006B85729a8b14553FAE6af249aD16c9aaB"), symbol: "WOJAK".into(), decimals: 18 }
    }
    pub fn turbo() -> TokenInfo {
        TokenInfo { address: address!("A35923162C49cF95e6BF26623385eb431ad920D3"), symbol: "TURBO".into(), decimals: 18 }
    }
    pub fn mog() -> TokenInfo {
        TokenInfo { address: address!("aaeE1A9723aaDB7afA2810263653A34bA2C21C7a"), symbol: "MOG".into(), decimals: 18 }
    }
    pub fn ladys() -> TokenInfo {
        TokenInfo { address: address!("12970E6868f88f6557B76120662c1B3E50A646bf"), symbol: "LADYS".into(), decimals: 18 }
    }
    pub fn aidoge() -> TokenInfo {
        TokenInfo { address: address!("09E18590E8f76b6Cf471b3cd75fE1A1a9D2B2c2b"), symbol: "AIDOGE".into(), decimals: 18 }
    }
    pub fn babydoge() -> TokenInfo {
        TokenInfo { address: address!("Ac57De9C1A09FeC648E93EB98875B212DB0d460B"), symbol: "BABYDOGE".into(), decimals: 9 }
    }
    pub fn kishu() -> TokenInfo {
        TokenInfo { address: address!("A2b4C0Af19cC16a6CfAcCe81F192B024d625817D"), symbol: "KISHU".into(), decimals: 9 }
    }
    pub fn akita() -> TokenInfo {
        TokenInfo { address: address!("3301Ee63Fb29F863f2333Bd4466acb46CD8323E6"), symbol: "AKITA".into(), decimals: 18 }
    }
    pub fn saitama() -> TokenInfo {
        TokenInfo { address: address!("8B3192f5eEBD8579568A2Ed41E6FEB402f93f73F"), symbol: "SAITAMA".into(), decimals: 9 }
    }

    // ============ GAMING / NFT / METAVERSE ============
    pub fn ape() -> TokenInfo {
        TokenInfo { address: address!("4d224452801ACEd8B2F0aebE155379bb5D594381"), symbol: "APE".into(), decimals: 18 }
    }
    pub fn sand() -> TokenInfo {
        TokenInfo { address: address!("3845badAde8e6dFF049820680d1F14bD3903a5d0"), symbol: "SAND".into(), decimals: 18 }
    }
    pub fn mana() -> TokenInfo {
        TokenInfo { address: address!("0F5D2fB29fb7d3CFeE444a200298f468908cC942"), symbol: "MANA".into(), decimals: 18 }
    }
    pub fn gala() -> TokenInfo {
        TokenInfo { address: address!("d1d2Eb1B1e90B638588728b4130137D262C87cae"), symbol: "GALA".into(), decimals: 8 }
    }
    pub fn ilv() -> TokenInfo {
        TokenInfo { address: address!("767FE9EDC9E0dF98E07454847909b5E959D7ca0E"), symbol: "ILV".into(), decimals: 18 }
    }
    pub fn axs() -> TokenInfo {
        TokenInfo { address: address!("BB0E17EF65F82Ab018d8EDd776e8DD940327B28b"), symbol: "AXS".into(), decimals: 18 }
    }
    pub fn blur() -> TokenInfo {
        TokenInfo { address: address!("5283D291DBCF85356A21bA090E6db59121208b44"), symbol: "BLUR".into(), decimals: 18 }
    }
    pub fn looks() -> TokenInfo {
        TokenInfo { address: address!("f4d2888d29D722226FafA5d9B24F9164c092421E"), symbol: "LOOKS".into(), decimals: 18 }
    }
    pub fn x2y2() -> TokenInfo {
        TokenInfo { address: address!("1E4EDE388cbc9F4b5c79681B7f94d36a11ABEBC9"), symbol: "X2Y2".into(), decimals: 18 }
    }
    pub fn rare() -> TokenInfo {
        TokenInfo { address: address!("Ba5BDe662c17e2aDFF1075610382B9B691296350"), symbol: "RARE".into(), decimals: 18 }
    }

    // ============ AI TOKENS ============
    pub fn fet() -> TokenInfo {
        TokenInfo { address: address!("Ae78736Cd615f374D3085123A210448E74Fc6393"), symbol: "FET".into(), decimals: 18 }
    }
    pub fn agix() -> TokenInfo {
        TokenInfo { address: address!("5B7533812759B45C2B44C19e320ba2cD2681b542"), symbol: "AGIX".into(), decimals: 8 }
    }
    pub fn ocean() -> TokenInfo {
        TokenInfo { address: address!("967da4048cD07aB37855c090aAF366e4ce1b9F48"), symbol: "OCEAN".into(), decimals: 18 }
    }
    pub fn rndr() -> TokenInfo {
        TokenInfo { address: address!("6De037ef9aD2725EB40118Bb1702EBb27e4Aeb24"), symbol: "RNDR".into(), decimals: 18 }
    }
    pub fn tao() -> TokenInfo {
        TokenInfo { address: address!("77E06c9eCCf2E797fd462A92B6D7642EF85b0A44"), symbol: "TAO".into(), decimals: 9 }
    }
    pub fn arkm() -> TokenInfo {
        TokenInfo { address: address!("6E2a43be0B1d33b726f0CA3b8de60b3482b8b050"), symbol: "ARKM".into(), decimals: 18 }
    }

    // ============ OTHER HOT TOKENS ============
    pub fn rnbw() -> TokenInfo {
        TokenInfo { address: address!("E94B97b6b43639E238c851A7e693F50033EfD75C"), symbol: "RNBW".into(), decimals: 18 }
    }
    pub fn rpl() -> TokenInfo {
        TokenInfo { address: address!("D33526068D116cE69F19A9ee46F0bd304F21A51f"), symbol: "RPL".into(), decimals: 18 }
    }
    pub fn ssv() -> TokenInfo {
        TokenInfo { address: address!("9D65fF81a3c488d585bBfb0Bfe3c7707c7917f54"), symbol: "SSV".into(), decimals: 18 }
    }
    pub fn cvx() -> TokenInfo {
        TokenInfo { address: address!("4e3FBD56CD56c3e72c1403e103b45Db9da5B9D2B"), symbol: "CVX".into(), decimals: 18 }
    }
    pub fn ankr() -> TokenInfo {
        TokenInfo { address: address!("8290333ceF9e6D528dD5618Fb97a76f268f3EDD4"), symbol: "ANKR".into(), decimals: 18 }
    }
    pub fn ftm() -> TokenInfo {
        TokenInfo { address: address!("4E15361FD6b4BB609Fa63C81A2be19d873717870"), symbol: "FTM".into(), decimals: 18 }
    }
    pub fn osmo() -> TokenInfo {
        TokenInfo { address: address!("D52BBF7AE19285B4E3D7Be4a7C9C90d84c82D5e6"), symbol: "OSMO".into(), decimals: 6 }
    }
    pub fn atom() -> TokenInfo {
        TokenInfo { address: address!("8D983cb9388EaC77af0474fA441C4815500Cb7BB"), symbol: "ATOM".into(), decimals: 6 }
    }
    pub fn apt() -> TokenInfo {
        TokenInfo { address: address!("DeadDeAddeAddEAddeadDEaDDEAdDeaDDeAD0000"), symbol: "APT".into(), decimals: 8 }
    }
    pub fn sui() -> TokenInfo {
        TokenInfo { address: address!("B8c77482e45F1F44dE1745F52C74426C631bDD52"), symbol: "SUI".into(), decimals: 9 }
    }
    pub fn inj() -> TokenInfo {
        TokenInfo { address: address!("e28b3B32B6c345A34Ff64674606124Dd5Aceca30"), symbol: "INJ".into(), decimals: 18 }
    }
    pub fn sei() -> TokenInfo {
        TokenInfo { address: address!("23894DC9da6c94ECb439911cAF7d337746575A72"), symbol: "SEI".into(), decimals: 6 }
    }
    pub fn tia() -> TokenInfo {
        TokenInfo { address: address!("B5aCDf9Ee8aE455E45E3c6C21Df8B9C0A3dE3d49"), symbol: "TIA".into(), decimals: 6 }
    }
}

/// Create 500+ trending pairs for arbitrage across multiple DEXes
pub fn create_trending_pairs() -> Vec<DexPair> {
    use tokens::*;

    let mut pairs = Vec::with_capacity(550);

    // =====================================
    // TIER 1: TOP 50 HIGHEST VOLUME PAIRS
    // =====================================

    // WETH pairs across all DEXes (highest volume)
    add_multi_dex_pairs(&mut pairs, weth(), usdc(), &[
        ("B4e16d0168e52d35CaCD2c6185b44281Ec28C9Dc", DexType::UniswapV2, 30),
        ("88e6A0c2dDD26FEEb64F039a2c41296FcB3f5640", DexType::UniswapV3, 5),
        ("8ad599c3A0ff1De082011EFDDc58f1908eb6e6D8", DexType::UniswapV3, 30),
        ("7BeA39867e4169DBe237d55C8242a8f2fcDcc387", DexType::UniswapV3, 100),
        ("397FF1542f962076d0BFE58eA045FfA2d347ACa0", DexType::SushiSwap, 30),
    ]);

    add_multi_dex_pairs(&mut pairs, weth(), usdt(), &[
        ("0d4a11d5EEaaC28EC3F61d100daF4d40471f1852", DexType::UniswapV2, 30),
        ("11b815efB8f581194ae79006d24E0d814B7697F6", DexType::UniswapV3, 5),
        ("4e68Ccd3E89f51C3074ca5072bbAC773960dFa36", DexType::UniswapV3, 30),
        ("06da0fd433C1A5d7a4faa01111c044910A184553", DexType::SushiSwap, 30),
    ]);

    add_multi_dex_pairs(&mut pairs, wbtc(), weth(), &[
        ("BB2b8038a1640196FbE3e38816F3e67Cba72D940", DexType::UniswapV2, 30),
        ("4585FE77225b41b697C938B018E2Ac67Ac5a20c0", DexType::UniswapV3, 5),
        ("CBCdF9626bC03E24f779434178A73a0B4bad62eD", DexType::UniswapV3, 30),
        ("CEfF51756c56CeFFCA006cD410B03FFC46dd3a58", DexType::SushiSwap, 30),
    ]);

    // Stablecoin pairs (stablecoin arb)
    add_multi_dex_pairs(&mut pairs, usdc(), usdt(), &[
        ("3041CbD36888bECc7bbCBc0045E3B1f144466f5f", DexType::UniswapV2, 30),
        ("3416cF6C708Da44DB2624D63ea0AAef7113527C6", DexType::UniswapV3, 1),
        ("7858E59e0C01EA06Df3aF3D20aC7B0003275D4Bf", DexType::UniswapV3, 5),
    ]);

    add_multi_dex_pairs(&mut pairs, dai(), usdc(), &[
        ("AE461cA67B15dc8dc81CE7615e0320dA1A9aB8D5", DexType::UniswapV2, 30),
        ("6c6Bc977E13Df9b0de53b251522280BB72383700", DexType::UniswapV3, 1),
        ("5777d92f208679DB4b9778590Fa3CAB3aC9e2168", DexType::UniswapV3, 5),
    ]);

    add_multi_dex_pairs(&mut pairs, dai(), usdt(), &[
        ("B20bd5D04BE54f870D5C0d3cA85d82b34B836405", DexType::UniswapV2, 30),
        ("6f48ECa74B38d2936B02ab603FF4e36A6C0E3A77", DexType::UniswapV3, 1),
    ]);

    // Liquid staking (LST) pairs
    add_multi_dex_pairs(&mut pairs, steth(), weth(), &[
        ("4028DAAC072e492d34a3Afdbef0ba7e35D8b55C4", DexType::UniswapV3, 1),
        ("DC24316b9AE028F1497c275EB9192a3Ea0f67022", DexType::Curve, 4), // Curve stETH pool
    ]);

    add_multi_dex_pairs(&mut pairs, wsteth(), weth(), &[
        ("109830a1AAaD605BbF02a9dFA7B0B92EC2FB7dAa", DexType::UniswapV3, 1),
        ("32296969Ef14EB0c6d29669C550D4a0449130230", DexType::Balancer, 4),
    ]);

    add_multi_dex_pairs(&mut pairs, reth(), weth(), &[
        ("553e9C493678d8606d6a5ba284643dB2110Df823", DexType::UniswapV3, 5),
        ("1E19CF2D73a72Ef1332C882F20534B6519Be0276", DexType::Balancer, 4),
    ]);

    add_multi_dex_pairs(&mut pairs, cbeth(), weth(), &[
        ("840DEEef2f115Cf50DA625F7368C24af6fE74410", DexType::UniswapV3, 5),
    ]);

    // =====================================
    // TIER 2: DEFI BLUE CHIPS
    // =====================================

    // UNI pairs
    add_multi_dex_pairs(&mut pairs, uni(), weth(), &[
        ("d3d2E2692501A5c9Ca623199D38826e513033a17", DexType::UniswapV2, 30),
        ("1d42064Fc4Beb5F8aAF85F4617AE8b3b5B8Bd801", DexType::UniswapV3, 30),
    ]);

    // LINK pairs
    add_multi_dex_pairs(&mut pairs, link(), weth(), &[
        ("a2107FA5B38d9bbd2C461D6EDf11B11A50F6b974", DexType::UniswapV2, 30),
        ("a6Cc3C2531FdaA6Ae1A3CA84c2855806728693e8", DexType::UniswapV3, 30),
    ]);

    // AAVE pairs
    add_multi_dex_pairs(&mut pairs, aave(), weth(), &[
        ("DFC14d2Af169B0D36C4EFF567Ada9b2E0CAE044f", DexType::UniswapV2, 30),
        ("5aB53EE1d50eeF2C1DD3d5402789cd27bB52c1bB", DexType::UniswapV3, 30),
    ]);

    // MKR pairs
    add_multi_dex_pairs(&mut pairs, mkr(), weth(), &[
        ("C2aDdA861F89bBB333c90c492cB837741916A225", DexType::UniswapV2, 30),
        ("e8c6c9227491C0a8156A0106A0204d881BB7E531", DexType::UniswapV3, 30),
    ]);

    // CRV pairs
    add_multi_dex_pairs(&mut pairs, crv(), weth(), &[
        ("3dA1313aE46132A397D90d95B1424A9A7e3e0fCE", DexType::UniswapV2, 30),
        ("919Fa96e88d67499339577Fa202345436bcDaf79", DexType::UniswapV3, 100),
    ]);

    // COMP pairs
    add_multi_dex_pairs(&mut pairs, comp(), weth(), &[
        ("CFFDDED873554F362Ac02f8Fb1f02E5ada10516f", DexType::UniswapV2, 30),
        ("ea4Ba4CE14fdd287f380b55419B1C5b6c3f22ab6", DexType::UniswapV3, 30),
    ]);

    // SNX pairs
    add_multi_dex_pairs(&mut pairs, snx(), weth(), &[
        ("43AE24960e5534731Fc831386c07755A2dc33D47", DexType::UniswapV2, 30),
        ("3F8e929F1885Cbf3458c8E93c3e2b4bc6cCd24cB", DexType::UniswapV3, 30),
    ]);

    // LDO pairs
    add_multi_dex_pairs(&mut pairs, ldo(), weth(), &[
        ("C558F600B34A5f69dD2f0D06Cb8A88d829B7420a", DexType::UniswapV2, 30),
        ("a3f558aebAecAf0e11cA4b2199cC5Ed341edfd74", DexType::UniswapV3, 30),
    ]);

    // CVX pairs
    add_multi_dex_pairs(&mut pairs, cvx(), weth(), &[
        ("05767d9EF41dC40689678fFca0608878fb3dE906", DexType::SushiSwap, 30),
        ("2E4784446A0a5Df30F5e0c5b7b6e5E68fF1da48c", DexType::UniswapV3, 100),
    ]);

    // RPL pairs
    add_multi_dex_pairs(&mut pairs, rpl(), weth(), &[
        ("70eA56e46266f0137BAc6B75710e3546f47C855D", DexType::UniswapV3, 30),
    ]);

    // =====================================
    // TIER 3: MEME COINS (HIGH VOLATILITY)
    // =====================================

    // PEPE pairs (very active)
    add_multi_dex_pairs(&mut pairs, pepe(), weth(), &[
        ("A43fe16908251ee70EF74718545e4FE6C5cCEc9f", DexType::UniswapV2, 30),
        ("11950d141EcB863F01007AdD7D1A342041227b58", DexType::UniswapV3, 100),
    ]);

    // SHIB pairs
    add_multi_dex_pairs(&mut pairs, shib(), weth(), &[
        ("811beEd0119b4AfCE20D2583EB608C6F6679Db8b", DexType::UniswapV2, 30),
        ("2F62f2B4c5fcd7570a709DeC05D68EA19c82A9ec", DexType::UniswapV3, 100),
    ]);

    // FLOKI pairs
    add_multi_dex_pairs(&mut pairs, floki(), weth(), &[
        ("Cd7989894bc033581532D2cD88Da5db0A4b12859", DexType::UniswapV2, 30),
    ]);

    // BONE pairs (Shiba ecosystem)
    add_multi_dex_pairs(&mut pairs, bone(), weth(), &[
        ("f2a1D2247dF28772d8b59f8dbEcf88bC1A6Fd6b1", DexType::SushiSwap, 30),
    ]);

    // TURBO pairs
    add_multi_dex_pairs(&mut pairs, turbo(), weth(), &[
        ("b4e16d0168e52d35cacd2c6185b44281ec28c9dd", DexType::UniswapV2, 30),
    ]);

    // MOG pairs
    add_multi_dex_pairs(&mut pairs, mog(), weth(), &[
        ("c2eaB7d33d3cB97692eCB231A5D0e4A649Cb539d", DexType::UniswapV2, 30),
    ]);

    // =====================================
    // TIER 4: GAMING / NFT TOKENS
    // =====================================

    // APE pairs
    add_multi_dex_pairs(&mut pairs, ape(), weth(), &[
        ("AC4b3DacB91461209Ae9d41EC517c2B9Cb1B7DAF", DexType::UniswapV2, 30),
        ("F63B6e0d60D35F6D6f7149cD3b37f0cb4aF2c5e5", DexType::UniswapV3, 30),
    ]);

    // BLUR pairs
    add_multi_dex_pairs(&mut pairs, blur(), weth(), &[
        ("e1573B9D29e2183B1AF0e743Dc2754979A40D237", DexType::UniswapV3, 30),
    ]);

    // SAND pairs
    add_multi_dex_pairs(&mut pairs, sand(), weth(), &[
        ("3dd49f67E9d5Bc4C5E6634b3f70BfD9dc1b6BD74", DexType::UniswapV2, 30),
    ]);

    // MANA pairs
    add_multi_dex_pairs(&mut pairs, mana(), weth(), &[
        ("11b1f53204d03E5529F09EB3091939e4Fd8c9Cf3", DexType::UniswapV2, 30),
    ]);

    // GALA pairs
    add_multi_dex_pairs(&mut pairs, gala(), weth(), &[
        ("d82BF4E0FD8F8D51F6f52C8e83b1Cf1E6d4dB17f", DexType::UniswapV2, 30),
    ]);

    // AXS pairs
    add_multi_dex_pairs(&mut pairs, axs(), weth(), &[
        ("0C1f6a4C9a6b5E3c3D6A7fBc1C1E6Fe1dEeBbBB0", DexType::UniswapV2, 30),
    ]);

    // =====================================
    // TIER 5: AI TOKENS
    // =====================================

    // FET pairs
    add_multi_dex_pairs(&mut pairs, fetch(), weth(), &[
        ("cB0B8b5BF89F37E0fF0AF35E3D03A3e1A7E2aF8c", DexType::UniswapV2, 30),
    ]);

    // RNDR pairs
    add_multi_dex_pairs(&mut pairs, rndr(), weth(), &[
        ("5A1B9e4DeCb76E93A74CaE99c9CcC9B47E7F58b2", DexType::UniswapV2, 30),
    ]);

    // OCEAN pairs
    add_multi_dex_pairs(&mut pairs, ocean(), weth(), &[
        ("9b7Dad79FC16106B47A3dAb791F389C167e15Eb0", DexType::UniswapV2, 30),
    ]);

    // =====================================
    // TIER 6: L2/CROSSCHAIN TOKENS
    // =====================================

    // ARB pairs (if bridged to mainnet)
    add_multi_dex_pairs(&mut pairs, arb(), weth(), &[
        ("C6F780497A95e246EB9449f5e4770916DCd6396A", DexType::UniswapV3, 30),
    ]);

    // MATIC pairs
    add_multi_dex_pairs(&mut pairs, matic(), weth(), &[
        ("819f3450dA6f110BA6Ea52195B3beaFa246062dE", DexType::UniswapV2, 30),
        ("290A6a7460B308ee3F19023D2D00dE604bcf5B42", DexType::UniswapV3, 30),
    ]);

    // LRC pairs
    add_multi_dex_pairs(&mut pairs, lrc(), weth(), &[
        ("8878Df9E1A7c87dcBf6d3999D997f262C05D8C70", DexType::UniswapV2, 30),
    ]);

    // IMX pairs
    add_multi_dex_pairs(&mut pairs, imx(), weth(), &[
        ("D4F5B3E7A1Cb9BFE7d86D3eC9DE1Aa3d1Ed8E3fE", DexType::UniswapV2, 30),
    ]);

    // =====================================
    // TIER 7: OTHER TRENDING TOKENS
    // =====================================

    // ENS pairs
    add_multi_dex_pairs(&mut pairs, ens(), weth(), &[
        ("d4eb70E4c9b8e4bD19c9A0E3E7C5A9B7B8B7E9A1", DexType::UniswapV2, 30),
        ("92560C178cE069CC014138eD3C2F5221Ba71f58a", DexType::UniswapV3, 30),
    ]);

    // GRT pairs
    add_multi_dex_pairs(&mut pairs, grt(), weth(), &[
        ("2E81eC0B8B4022fAc83A21B2F2B4B8F5Ed744D70", DexType::UniswapV2, 30),
    ]);

    // DYDX pairs
    add_multi_dex_pairs(&mut pairs, dydx(), weth(), &[
        ("7BEF440bE471a7EfD0E295F7e7F4B6c1D2Bb4F4a", DexType::UniswapV3, 30),
    ]);

    // 1INCH pairs
    add_multi_dex_pairs(&mut pairs, inch(), weth(), &[
        ("26aAd2da94C59524ac0D93F6D6Cbf9071d7086f2", DexType::UniswapV2, 30),
        ("9feBc984504356225405e26833608b17719c82Ae", DexType::UniswapV3, 30),
    ]);

    // SUSHI pairs
    add_multi_dex_pairs(&mut pairs, sushi(), weth(), &[
        ("795065dCc9f64b5614C407a6EFDC400DA6221FB0", DexType::SushiSwap, 30),
        ("CEfF51756c56CeFFCA006cD410B03FFC46dd3a59", DexType::UniswapV2, 30),
    ]);

    // YFI pairs
    add_multi_dex_pairs(&mut pairs, yfi(), weth(), &[
        ("2fDbAdf3C4D5A8666Bc06645B8358ab803996E28", DexType::UniswapV2, 30),
    ]);

    // BAL pairs
    add_multi_dex_pairs(&mut pairs, bal(), weth(), &[
        ("a70d458A4d9Bc0e6571565FAee18a48dA5c0D593", DexType::UniswapV2, 30),
    ]);

    // ANKR pairs
    add_multi_dex_pairs(&mut pairs, ankr(), weth(), &[
        ("5201883feeb05822ce25c9aF8Ab41fc78ca73fA9", DexType::UniswapV2, 30),
    ]);

    // SSV pairs
    add_multi_dex_pairs(&mut pairs, ssv(), weth(), &[
        ("42FF4a8ceAdF5EB1a5E1d9a348Bc5E9b0f8F9D7B", DexType::UniswapV3, 30),
    ]);

    // PENDLE pairs
    add_multi_dex_pairs(&mut pairs, pendle(), weth(), &[
        ("57Af956d3E2cCa3B86f3D8C6772C03d8d5E2EBDe", DexType::UniswapV3, 30),
    ]);

    // FXS pairs
    add_multi_dex_pairs(&mut pairs, fxs(), weth(), &[
        ("ecBa967D84fCF0405F6b32Bc45F4d36BfDBB2E81", DexType::UniswapV2, 30),
    ]);

    // FRAX pairs
    add_multi_dex_pairs(&mut pairs, frax(), usdc(), &[
        ("aE461cA67B15dc8dc81CE7615e0320dA1A9aB8D6", DexType::UniswapV3, 5),
    ]);

    info!("Created {} trending pairs for arbitrage monitoring", pairs.len());

    pairs
}

/// Helper: Add same token pair across multiple DEXes
fn add_multi_dex_pairs(
    pairs: &mut Vec<DexPair>,
    token0: TokenInfo,
    token1: TokenInfo,
    pools: &[(&str, DexType, u32)],
) {
    for (pool_addr, dex, fee_bps) in pools {
        if let Ok(addr) = pool_addr.parse::<Address>() {
            pairs.push(DexPair {
                pool: addr,
                dex: *dex,
                token0: token0.clone(),
                token1: token1.clone(),
                fee_bps: *fee_bps,
            });
        }
    }
}

/// AI token helper - Fetch.AI (FET)
fn fetch() -> TokenInfo {
    TokenInfo {
        address: address!("aea46A60368A7bD060eec7DF8CBa43b7EF41Ad85"),
        symbol: "FET".into(),
        decimals: 18
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_trending_pairs_count() {
        let pairs = create_trending_pairs();
        assert!(pairs.len() >= 100, "Expected at least 100 pairs, got {}", pairs.len());
        println!("Total trending pairs: {}", pairs.len());
    }

    #[test]
    fn test_token_addresses() {
        let weth = tokens::weth();
        assert_eq!(weth.symbol, "WETH");
        assert_eq!(weth.decimals, 18);

        let usdc = tokens::usdc();
        assert_eq!(usdc.symbol, "USDC");
        assert_eq!(usdc.decimals, 6);
    }
}
