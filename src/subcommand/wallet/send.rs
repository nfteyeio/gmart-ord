use {super::*, crate::subcommand::wallet::transaction_builder::Target, crate::wallet::Wallet};

#[derive(Debug, Parser, Clone)]
pub(crate) struct Send {
  address: Address<NetworkUnchecked>,
  outgoing: Outgoing,
  #[arg(
    long,
    help = "Consider spending outpoint <UTXO>, even if it is unconfirmed or contains inscriptions"
  )]
  utxo: Vec<OutPoint>,
  #[clap(
    long,
    help = "Only spend outpoints given with --utxo when sending inscriptions or satpoints"
  )]
  pub(crate) coin_control: bool,
  #[arg(long, help = "Use fee rate of <FEE_RATE> sats/vB")]
  fee_rate: FeeRate,
  #[arg(
    long,
    help = "Target amount of postage to include with sent inscriptions. Default `10000sat`"
  )]
  pub(crate) postage: Option<Amount>,
  #[clap(long, help = "Require this utxo to be spent. Useful for forcing CPFP.")]
  pub(crate) force_input: Vec<OutPoint>,
}

#[derive(Serialize, Deserialize)]
pub struct Output {
  pub transaction: Txid,
}

impl Send {
  pub(crate) fn run(self, options: Options) -> SubcommandResult {
    let address = self
      .address
      .clone()
      .require_network(options.chain().network())?;

    let index = Index::open(&options)?;
    index.update()?;

    let chain = options.chain();

    let client = options.bitcoin_rpc_client_for_wallet_command(false)?;

    let mut unspent_outputs = if self.coin_control {
      BTreeMap::new()
    } else if options.ignore_outdated_index {
      return Err(anyhow!(
        "--ignore-outdated-index only works in conjunction with --coin-control when sending"
      ));
    } else {
      index.get_unspent_outputs(Wallet::load(&options)?)?
    };

    for outpoint in &self.utxo {
      unspent_outputs.insert(
        *outpoint,
        Amount::from_sat(
          client.get_raw_transaction(&outpoint.txid, None)?.output[outpoint.vout as usize].value,
        ),
      );
    }

    let wallet = Wallet::load(&options)?;

    let unspent_outputs = index.get_unspent_outputs(wallet)?;

    let locked_outputs = index.get_locked_outputs(wallet)?;

    let inscriptions = index.get_inscriptions(&unspent_outputs)?;

    let runic_outputs =
      index.get_runic_outputs(&unspent_outputs.keys().cloned().collect::<Vec<OutPoint>>())?;

    let satpoint = match self.outgoing {
      Outgoing::SatPoint(satpoint) => {
        for inscription_satpoint in inscriptions.keys() {
          if satpoint == *inscription_satpoint {
            bail!("inscriptions must be sent by inscription ID");
          }
        }

        ensure!(
          !runic_outputs.contains(&satpoint.outpoint),
          "runic outpoints may not be sent by satpoint"
        );

        satpoint
      }
      Outgoing::InscriptionId(id) => index
        .get_inscription_satpoint_by_id(id)?
        .ok_or_else(|| anyhow!("Inscription {id} not found"))?,
      Outgoing::Amount(amount) => {
        if self.coin_control || !self.utxo.is_empty() {
          bail!("--coin_control and --utxo don't work when sending cardinals");
        }
        Self::lock_inscriptions(&client, inscriptions, runic_outputs, unspent_outputs)?;
        let txid = Self::send_amount(&client, amount, address, self.fee_rate.n())?;
        return Ok(Box::new(Output { transaction: txid }));
      }
    };

    let change = [
      get_change_address(&client, chain)?,
      get_change_address(&client, chain)?,
    ];

    let postage = if let Some(postage) = self.postage {
      Target::ExactPostage(postage)
    } else {
      Target::Postage
    };

    let unsigned_transaction = TransactionBuilder::new(
      satpoint,
      inscriptions,
      unspent_outputs,
      locked_outputs,
      runic_outputs,
      address.clone(),
      change,
      self.fee_rate,
      postage,
      self.force_input,
    )
    .build_transaction()?;

    let signed_tx = client
      .sign_raw_transaction_with_wallet(&unsigned_transaction, None, None)?
      .hex;

    let txid = client.send_raw_transaction(&signed_tx)?;

    Ok(Box::new(Output { transaction: txid }))
  }

  fn lock_inscriptions(
    client: &Client,
    inscriptions: BTreeMap<SatPoint, InscriptionId>,
    runic_outputs: BTreeSet<OutPoint>,
    unspent_outputs: BTreeMap<OutPoint, bitcoin::Amount>,
  ) -> Result {
    let all_inscription_outputs = inscriptions
      .keys()
      .map(|satpoint| satpoint.outpoint)
      .collect::<HashSet<OutPoint>>();

    let locked_outputs = unspent_outputs
      .keys()
      .filter(|utxo| all_inscription_outputs.contains(utxo))
      .chain(runic_outputs.iter())
      .cloned()
      .collect::<Vec<OutPoint>>();

    if !client.lock_unspent(&locked_outputs)? {
      bail!("failed to lock UTXOs");
    }

    Ok(())
  }

  fn send_amount(client: &Client, amount: Amount, address: Address, fee_rate: f64) -> Result<Txid> {
    Ok(client.call(
      "sendtoaddress",
      &[
        address.to_string().into(), //  1. address
        amount.to_btc().into(),     //  2. amount
        serde_json::Value::Null,    //  3. comment
        serde_json::Value::Null,    //  4. comment_to
        serde_json::Value::Null,    //  5. subtractfeefromamount
        serde_json::Value::Null,    //  6. replaceable
        serde_json::Value::Null,    //  7. conf_target
        serde_json::Value::Null,    //  8. estimate_mode
        serde_json::Value::Null,    //  9. avoid_reuse
        fee_rate.into(),            // 10. fee_rate
      ],
    )?)
  }
}
