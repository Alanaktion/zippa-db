package main

import (
	"log"
	"os"

	"github.com/spf13/viper"
)

func configInit() {
	// Create the app config path
	home, err := os.UserHomeDir()
	if err != nil {
		log.Fatal("Unable to get user home directory: ", err)
	}
	configPath := home + "/.config/zippa-db/"
	err = os.MkdirAll(configPath, os.ModePerm)
	if err != nil {
		log.Fatal("Failed to create path for config: ", err)
	}

	// Create an empty file manually. This *would* be done with
	// viper.SafeWriteConfig() but that function is just straight up not coded
	// correctly so it always errors.
	file, err := os.OpenFile(configPath+"config.toml", os.O_RDONLY|os.O_CREATE, 0666)
	if err != nil {
		log.Fatal("Failed to initialize config file: ", err)
	}
	file.Close()

	// Initialize Viper
	viper.SetDefault("connections", make(map[string]interface{}))

	viper.SetConfigType("toml")
	viper.SetConfigName("config")
	viper.AddConfigPath(configPath)

	err = viper.ReadInConfig()
	if err != nil {
		log.Println("Unable to read config: ", err)
	}
}

func configSave() {
	viper.WriteConfig()
}
