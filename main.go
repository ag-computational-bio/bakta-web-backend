package main

import (
	"log"

	"github.com/ag-computational-bio/bakta-web-backend/endpoints"
	"github.com/jessevdk/go-flags"
	"github.com/spf13/viper"
)

var opts struct {
	ConfigFile string `short:"c" long:"configfile" description:"File of the config file" default:"config/local-config.yaml"`
}

//Version Version tag
var Version string

func main() {
	// Enable line numbers in logging
	log.SetFlags(log.LstdFlags | log.Lshortfile)

	_, err := flags.Parse(&opts)
	if err != nil {
		log.Fatalln(err.Error())
	}

	viper.SetConfigFile(opts.ConfigFile)
	err = viper.ReadInConfig()
	if err != nil {
		log.Fatalln(err.Error())
	}

	err = endpoints.RunGrpcJobServer()
	if err != nil {
		log.Fatalln(err.Error())
	}
}
